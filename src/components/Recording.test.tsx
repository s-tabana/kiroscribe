import { describe, expect, it } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { BackendContext } from "../lib/backend";
import { createMemoryBackend, type MemoryBackend } from "../lib/memoryBackend";
import { groupSegments } from "./TranscriptPanel";
import { levelToBars } from "./LevelMeter";
import { elapsedMs, formatElapsed } from "../lib/elapsed";
import type { Segment } from "../lib/types";

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
  it("録音を開始すると文字起こしが表示され、停止できる", async () => {
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

  it("API キーがないと案内を出し、設定画面へ誘導する", async () => {
    const user = userEvent.setup();
    await setup(false);
    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("API キーが設定されていません（OpenAI）");
    await user.click(within(alert).getByRole("button", { name: "設定を開く" }));
    expect(await screen.findByRole("dialog", { name: "設定" })).toBeInTheDocument();
  });

  it("録音中に起きたエラーを知らせる", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const noteId = await currentNoteId(backend);
    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");
    act(() => backend.emitError(noteId, "相手の発話（10:00:00〜）を文字起こしできませんでした: 接続できませんでした"));
    expect(await screen.findByRole("alert")).toHaveTextContent("文字起こしできませんでした");
  });

  it("録音中のノートは削除できない", async () => {
    const user = userEvent.setup();
    await setup();
    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await screen.findByRole("timer");
    await user.click(screen.getByRole("button", { name: "ノートを削除" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("録音中のノートは削除できません");
  });

  it("別のノートを開くと、録音中のノートへ戻る案内が出る", async () => {
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

  it("同じ話者の続く発話をまとめる", () => {
    const groups = groupSegments([
      seg(1, "them", 0),
      seg(2, "them", 4_000),
      seg(3, "me", 8_000),
      seg(4, "me", 200_000), // 1 分以上空いたら別のまとまり
    ]);
    expect(groups.map((g) => g.map((s) => s.id))).toEqual([[1, 2], [3], [4]]);
  });

  it("入力レベルを 0〜5 本に変換する", () => {
    expect(levelToBars(0)).toBe(0);
    expect(levelToBars(0.001)).toBe(0);
    expect(levelToBars(0.01)).toBe(1); // -40dB
    expect(levelToBars(0.05)).toBe(3); // -26dB
    expect(levelToBars(0.3)).toBe(5);
    expect(levelToBars(1)).toBe(5);
  });

  it("一時停止を除いた録音時間を表示する", () => {
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
