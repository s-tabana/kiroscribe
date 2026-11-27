import { describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { BackendContext } from "../lib/backend";
import { createMemoryBackend, type MemoryBackend } from "../lib/memoryBackend";
import { groupSegments, roundLabel, splitRounds } from "./TranscriptPanel";
import { levelToBars } from "./LevelMeter";
import { elapsedMs, formatElapsed } from "../lib/elapsed";
import type { Segment, TranscriptBreak } from "../lib/types";

async function setup(withKey = true) {
  const backend = createMemoryBackend({ seed: true, confirm: () => true });
  if (withKey) await backend.setApiKey("openai", "sk-test");
  render(
    <BackendContext.Provider value={backend}>
      <App />
    </BackendContext.Provider>,
  );
  await screen.findByDisplayValue("プロダクト定例");
  return backend;
}

function currentNoteId(backend: MemoryBackend) {
  return backend.listNotes().then((notes) => notes[0].id);
}

describe("録音", () => {
  it("[REC-001][REC-009][REC-014][TRN-016][TRN-021][NOTE-006] 録音を開始すると文字起こしが表示され、停止できる", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const noteId = await currentNoteId(backend);

    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    expect(await screen.findByRole("timer")).toHaveTextContent("録音中");
    // 録音が始まると文字起こしパネルが開く
    const panel = screen.getByRole("complementary", { name: "文字起こし" });
    expect(within(panel).getByText("ライブ")).toBeInTheDocument();

    act(() => {
      backend.emitSegment(noteId, "them", "リリース日を確認しましょう。");
      backend.emitSegment(noteId, "me", "12月5日で進めます。");
      backend.emitSegment("other-note", "me", "別ノートの発話");
    });
    expect(within(panel).getByText("リリース日を確認しましょう。")).toBeInTheDocument();
    expect(within(panel).getByText("12月5日で進めます。")).toBeInTheDocument();
    expect(within(panel).queryByText("別ノートの発話")).not.toBeInTheDocument();
    expect(within(panel).getByText("相手")).toBeInTheDocument();

    // サイドバーに録音中の印
    expect(screen.getByLabelText("録音中")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "一時停止" }));
    expect(await screen.findByRole("timer")).toHaveTextContent("一時停止中");
    await user.click(screen.getByRole("button", { name: "再開" }));
    expect(await screen.findByRole("timer")).toHaveTextContent("録音中");

    await user.click(screen.getByRole("button", { name: "停止" }));
    expect(await screen.findByRole("button", { name: "録音を再開" })).toBeInTheDocument();
    expect(screen.queryByLabelText("録音中")).not.toBeInTheDocument();
    // 文字起こしは残る
    expect(screen.getByRole("button", { name: /文字起こし\s*2/ })).toBeInTheDocument();
  });

  it("[REC-002][ERR-001][ERR-002] API キーがないと案内を出し、設定画面へ誘導する", async () => {
    const user = userEvent.setup();
    await setup(false);
    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("API キーが設定されていません（OpenAI）");
    await user.click(within(alert).getByRole("button", { name: "設定を開く" }));
    expect(await screen.findByRole("dialog", { name: "設定" })).toBeInTheDocument();
  });

  it("[ERR-010][REC-021] 録音中に起きたエラーを知らせる", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const noteId = await currentNoteId(backend);
    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");
    act(() => backend.emitError(noteId, "相手の発話（10:00:00〜）を文字起こしできませんでした: 接続できませんでした"));
    expect(await screen.findByRole("alert")).toHaveTextContent("文字起こしできませんでした");
  });

  it("[NOTE-016] 録音中のノートは削除できない", async () => {
    const user = userEvent.setup();
    await setup();
    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");
    await user.click(screen.getByRole("button", { name: "ノートを削除" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("録音中のノートは削除できません");
  });

  it("[REC-013] 別のノートを開くと、録音中のノートへ戻る案内が出る", async () => {
    const user = userEvent.setup();
    await setup();
    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");

    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    await user.click(within(list).getByText("採用面談（バックエンド）"));
    await screen.findByDisplayValue("採用面談（バックエンド）");
    expect(screen.getByText(/別のノートで録音中/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "録音を開始" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "そのノートを開く" }));
    expect(await screen.findByDisplayValue("プロダクト定例")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("timer")).toHaveTextContent("録音中"));
  });
});

describe("表示用の補助関数", () => {
  const seg = (id: number, speaker: "me" | "them", startedAt: number): Segment => ({
    id,
    noteId: "n",
    speaker,
    startedAt,
    endedAt: startedAt + 3_000,
    text: `t${id}`,
  });

  it("[TRN-018] 同じ話者の続く発話をまとめる", () => {
    const groups = groupSegments([
      seg(1, "them", 0),
      seg(2, "them", 4_000),
      seg(3, "me", 8_000),
      seg(4, "me", 200_000), // 1 分以上空いたら別のまとまり
    ]);
    expect(groups.map((g) => g.map((s) => s.id))).toEqual([[1, 2], [3], [4]]);
  });

  it("[REC-012] 入力レベルを 0〜5 本に変換する", () => {
    expect(levelToBars(0)).toBe(0);
    expect(levelToBars(0.001)).toBe(0);
    expect(levelToBars(0.01)).toBe(1); // -40dB
    expect(levelToBars(0.05)).toBe(3); // -26dB
    expect(levelToBars(0.3)).toBe(5);
    expect(levelToBars(1)).toBe(5);
  });

  it("[REC-011][REC-007] 一時停止を除いた録音時間を表示する", () => {
    const status = {
      noteId: "n",
      state: "recording" as const,
      elapsedMs: 60_000,
      runningSince: 1_000,
      sources: [],
    };
    expect(formatElapsed(elapsedMs(status, 8_500))).toBe("1:07");
    expect(formatElapsed(elapsedMs({ ...status, runningSince: null }, 999_999))).toBe("1:00");
    expect(formatElapsed(3_725_000)).toBe("1:02:05");
  });
});

describe("録音: 文字起こしのパネル", () => {
  it("[REC-015][TRN-023][TRN-024] 文字起こしのあるノートで録音を再開すると、区切りの後に新しい発話が加わる", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const noteId = await currentNoteId(backend);

    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");
    act(() => {
      backend.emitSegment(noteId, "them", "一回目の録音の発話");
    });
    await user.click(screen.getByRole("button", { name: "停止" }));

    // 文字起こしが 1 件以上あるノートでは、開始のボタンが「録音を再開」になる
    await user.click(await screen.findByRole("button", { name: "録音を再開" }));
    expect(await screen.findByRole("timer")).toHaveTextContent("録音中");
    expect((await backend.getRecordingStatus()).noteId).toBe(noteId);
    act(() => {
      backend.emitSegment(noteId, "me", "二回目の録音の発話");
    });

    const panel = screen.getByRole("complementary", { name: "文字起こし" });
    const texts = within(panel).getAllByText(/回目の録音の発話/).map((element) => element.textContent);
    expect(texts).toEqual(["一回目の録音の発話", "二回目の録音の発話"]);
    expect((await backend.listSegments(noteId)).map((segment) => segment.text)).toEqual(texts);
    // 議事録のないノートでは聞かずに続きとして録音し、再開した位置に区切りを入れる
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    const [added] = await backend.listTranscriptBreaks(noteId);
    expect(added.round).toBe(2);
    const heading = within(panel).getByRole("heading", { name: roundLabel(2, added.startedAt) });
    expect(heading).toHaveTextContent(/^2回目の録音（\d{1,2}:\d{2}〜）$/);
    // 区切りは、1 回目の発話と 2 回目の発話の間にある
    const order = [...panel.querySelectorAll(".utterance-bubble p, .transcript-break")].map((e) => e.textContent);
    expect(order).toEqual(["一回目の録音の発話", heading.textContent, "二回目の録音の発話"]);

    // 何も話さずに止めた回の区切りは残らない
    await user.click(screen.getByRole("button", { name: "停止" }));
    await user.click(await screen.findByRole("button", { name: "録音を再開" }));
    expect(await within(panel).findByRole("heading", { name: /^3回目の録音/ })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "停止" }));
    await screen.findByRole("button", { name: "録音を再開" });
    await waitFor(() => expect(within(panel).queryByRole("heading", { name: /^3回目の録音/ })).toBeNull());
    expect((await backend.listTranscriptBreaks(noteId)).map((b) => b.round)).toEqual([2]);
  });

  it("[REC-018] 相手の声を取り込んでいないときは、文字起こしのパネルにその旨を表示する", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend({ seed: true, settings: { captureSystemAudio: false } });
    await backend.setApiKey("openai", "sk-test");
    render(
      <BackendContext.Provider value={backend}>
        <App />
      </BackendContext.Provider>,
    );
    await screen.findByDisplayValue("プロダクト定例");
    const noteId = await currentNoteId(backend);

    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");
    const panel = screen.getByRole("complementary", { name: "文字起こし" });
    expect(within(panel).getByText("相手の声は取り込んでいません（マイクのみ）")).toBeInTheDocument();
    // 入力の大きさのバーも「自分」だけ
    expect(screen.getByTitle("自分の入力レベル")).toBeInTheDocument();
    expect(screen.queryByTitle("相手の入力レベル")).not.toBeInTheDocument();

    // 取り込めなかった理由は、知らせとして表示する
    act(() => backend.emitError(noteId, "相手の声は取り込まず、マイクだけで録音します（音声の出力デバイスが見つかりません）"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "相手の声は取り込まず、マイクだけで録音します（音声の出力デバイスが見つかりません）",
    );

    // 録音を止めると、注記は出さない
    await user.click(screen.getByRole("button", { name: "停止" }));
    await screen.findByRole("button", { name: "録音を開始" });
    expect(within(panel).queryByText("相手の声は取り込んでいません（マイクのみ）")).not.toBeInTheDocument();
  });

  it("[REC-018] 相手の声も取り込んでいるときは、注記を出さない", async () => {
    const user = userEvent.setup();
    await setup();
    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");
    expect(screen.getByTitle("相手の入力レベル")).toBeInTheDocument();
    expect(screen.queryByText("相手の声は取り込んでいません（マイクのみ）")).not.toBeInTheDocument();
  });

  it("[TRN-016] 同じ発話が二重に届いても、1 回だけ表示する", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend({ seed: true });
    await backend.setApiKey("openai", "sk-test");
    const subscribe = backend.onSegment.bind(backend);
    const listeners: Array<(segment: Segment) => void> = [];
    vi.spyOn(backend, "onSegment").mockImplementation((cb) => {
      listeners.push(cb);
      return subscribe(cb);
    });
    render(
      <BackendContext.Provider value={backend}>
        <App />
      </BackendContext.Provider>,
    );
    await screen.findByDisplayValue("プロダクト定例");
    const noteId = await currentNoteId(backend);
    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");

    let segment!: Segment;
    act(() => {
      segment = backend.emitSegment(noteId, "me", "一度だけ表示する発話");
    });
    // 同じ発話（同じ ID）がもう一度届く
    act(() => listeners.forEach((listener) => listener({ ...segment })));

    const panel = screen.getByRole("complementary", { name: "文字起こし" });
    expect(within(panel).getAllByText("一度だけ表示する発話")).toHaveLength(1);
    expect(panel.querySelectorAll(".utterance-bubble p")).toHaveLength(1);
  });

  it("[TRN-020] 文字起こしがまだないときは、録音しているかどうかで案内を変える", async () => {
    const user = userEvent.setup();
    await setup();

    await user.click(screen.getByRole("button", { name: "文字起こし" }));
    const panel = screen.getByRole("complementary", { name: "文字起こし" });
    expect(within(panel).getByText("録音を開始すると、会話の文字起こしがここに表示されます。")).toBeInTheDocument();
    expect(within(panel).queryByText("ライブ")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");
    expect(within(panel).getByText("話し始めると、数秒後にここへ表示されます。")).toBeInTheDocument();
  });
});

/** 文字起こしと議事録のあるノートを用意してから、画面を開く */
async function setupWithMinutes() {
  const backend = createMemoryBackend({ seed: true, confirm: () => true });
  await backend.setApiKey("openai", "sk-test");
  const noteId = await currentNoteId(backend);
  await backend.startRecording(noteId);
  backend.emitSegment(noteId, "them", "一回目の会議の発話");
  await backend.stopRecording();
  await backend.updateNote(noteId, { enhancedNotes: "## 決定事項\n- リリースは 12/5" });
  render(
    <BackendContext.Provider value={backend}>
      <App />
    </BackendContext.Provider>,
  );
  await screen.findByDisplayValue("プロダクト定例");
  return { backend, noteId };
}

describe("録音: 議事録のあるノートでの再開", () => {
  it("[REC-027][TRN-024] 「続きとして録音」を選ぶと、同じノートで区切りを付けて録音する", async () => {
    const user = userEvent.setup();
    const { backend, noteId } = await setupWithMinutes();

    await user.click(screen.getByRole("button", { name: "録音を再開" }));
    const dialog = screen.getByRole("dialog", { name: "録音の再開" });
    expect(dialog).toHaveTextContent("このノートには議事録があります。同じ会議の続きとして録音しますか？");
    // 聞いている間は、録音を始めない
    expect((await backend.getRecordingStatus()).state).toBe("idle");
    expect(within(dialog).getByRole("button", { name: "続きとして録音" })).toHaveFocus();

    await user.click(within(dialog).getByRole("button", { name: "続きとして録音" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(await screen.findByRole("timer")).toHaveTextContent("録音中");
    expect((await backend.getRecordingStatus()).noteId).toBe(noteId);
    const panel = screen.getByRole("complementary", { name: "文字起こし" });
    expect(await within(panel).findByRole("heading", { name: /^2回目の録音（/ })).toBeInTheDocument();
    act(() => {
      backend.emitSegment(noteId, "me", "二回目の会議の発話");
    });
    expect(within(panel).getByText("二回目の会議の発話")).toBeInTheDocument();
    expect((await backend.listTranscriptBreaks(noteId)).map((b) => b.round)).toEqual([2]);
  });

  it("[REC-027] 「新しいノートで録音」を選ぶと、新しいノートを作って開き、そこで録音する", async () => {
    const user = userEvent.setup();
    const { backend, noteId } = await setupWithMinutes();
    const before = (await backend.listNotes()).length;

    await user.click(screen.getByRole("button", { name: "録音を再開" }));
    await user.click(screen.getByRole("button", { name: "新しいノートで録音" }));
    expect(await screen.findByRole("timer")).toHaveTextContent("録音中");
    const notes = await backend.listNotes();
    expect(notes).toHaveLength(before + 1);
    const status = await backend.getRecordingStatus();
    expect(status.noteId).toBe(notes[0].id);
    expect(status.noteId).not.toBe(noteId);
    // 開いているのは新しいノート（空のタイトル）。元のノートには区切りも発話も足さない
    expect(screen.getByLabelText("タイトル")).toHaveValue("");
    expect(await backend.listTranscriptBreaks(noteId)).toEqual([]);
    expect(await backend.listSegments(noteId)).toHaveLength(1);
  });

  it("[REC-027] 取り消す・閉じると、録音を始めない", async () => {
    const user = userEvent.setup();
    const { backend, noteId } = await setupWithMinutes();

    await user.click(screen.getByRole("button", { name: "録音を再開" }));
    await user.click(screen.getByRole("button", { name: "取り消す" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "録音を再開" }));
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    expect((await backend.getRecordingStatus()).state).toBe("idle");
    expect(await backend.listTranscriptBreaks(noteId)).toEqual([]);
    expect((await backend.listNotes()).length).toBe(3);
    expect(screen.getByRole("button", { name: "録音を再開" })).toBeEnabled();
  });
});

describe("録音: 文字起こしの区切り", () => {
  const segment = (id: number, startedAt: number): Segment => ({
    id,
    noteId: "n",
    speaker: "me",
    startedAt,
    endedAt: startedAt + 500,
    text: `発話${id}`,
  });
  const brk = (id: number, round: number, startedAt: number): TranscriptBreak => ({
    id,
    noteId: "n",
    round,
    startedAt,
  });

  it("[TRN-024] 発話を区切りで回ごとに分け、見出しは「2回目の録音（14:30〜）」の形にする", () => {
    const segments = [segment(1, 1_000), segment(2, 2_000), segment(3, 10_000)];
    expect(splitRounds(segments, []).map((r) => r.segments.length)).toEqual([3]);
    const rounds = splitRounds(segments, [brk(2, 3, 20_000), brk(1, 2, 9_000)]);
    expect(rounds.map((r) => [r.start?.round ?? 1, r.segments.map((s) => s.id)])).toEqual([
      [1, [1, 2]],
      [2, [3]],
      [3, []],
    ]);
    const at = new Date(2026, 10, 27, 14, 30).getTime();
    expect(roundLabel(2, at)).toBe("2回目の録音（14:30〜）");
  });
});
