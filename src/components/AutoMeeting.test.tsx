import { describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { MeetingPopup } from "./MeetingPopup";
import { SettingsDialog } from "./SettingsDialog";
import { BackendContext, type Backend } from "../lib/backend";
import { createMemoryBackend, type MemoryBackend } from "../lib/memoryBackend";

function renderWith(backend: Backend, ui: React.ReactElement) {
  render(<BackendContext.Provider value={backend}>{ui}</BackendContext.Provider>);
}

async function meetingBackend(): Promise<MemoryBackend> {
  const backend = createMemoryBackend({ seed: true, confirm: () => true, streamDelayMs: 0 });
  await backend.setApiKey("openai", "sk-test");
  await backend.setApiKey("anthropic", "sk-ant-test");
  return backend;
}

describe("会議検出のポップアップ", () => {
  it("[POP-003][POP-004][POP-005][POP-008][REC-016] 録音中の表示から停止でき、停止後は議事録の作成へ進める", async () => {
    const user = userEvent.setup();
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("Google Meet 11/20 15:04");
    await backend.startRecording(note.id);
    backend.setPopup({ kind: "recording", app: "Google Meet", noteId: note.id, title: note.title });
    const openNote = vi.spyOn(backend, "openNoteFromPopup");

    renderWith(backend, <MeetingPopup />);
    const popup = await screen.findByRole("alertdialog", { name: "会議の自動録音" });
    expect(within(popup).getByText("Google Meet")).toBeInTheDocument();
    expect(within(popup).getByText(/自動で録音・文字起こしを始めました/)).toBeInTheDocument();
    expect(await within(popup).findByRole("timer")).toBeInTheDocument();

    await user.click(within(popup).getByRole("button", { name: "停止" }));
    expect(await within(popup).findByText(/録音を停止しました/)).toBeInTheDocument();
    expect((await backend.getRecordingStatus()).state).toBe("idle");

    await user.click(within(popup).getByRole("button", { name: "議事録を作成" }));
    expect(openNote).toHaveBeenCalledWith(note.id, true);
  });

  it("[POP-002][POP-005][MTG-016] 会議の終了を知らせる（Rust 側からの更新を反映）", async () => {
    const backend = await meetingBackend();
    backend.setPopup({ kind: "recording", app: "Slack ハドル", noteId: "n1", title: "Slack ハドル 11/20 10:00" });
    renderWith(backend, <MeetingPopup />);
    await screen.findByText(/自動で録音・文字起こしを始めました/);

    act(() => backend.setPopup({ kind: "finishing", app: "Slack ハドル", noteId: "n1", title: "t" }));
    expect(await screen.findByText(/文字起こしを仕上げています/)).toBeInTheDocument();
    act(() => backend.setPopup({ kind: "stopped", app: "Slack ハドル", noteId: "n1", title: "Slack ハドル 11/20 10:00" }));
    expect(await screen.findByText("録音を停止しました。「Slack ハドル 11/20 10:00」に保存しています。")).toBeInTheDocument();
  });

  it("[POP-006][POP-009][MTG-014] API キーがないときは設定へ案内する", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend();
    backend.setPopup({ kind: "error", app: "Zoom", message: "OpenAI の API キーが設定されていないため、自動で録音できませんでした" });
    const openSettings = vi.spyOn(backend, "openSettingsFromPopup");
    renderWith(backend, <MeetingPopup />);
    expect(await screen.findByText(/API キーが設定されていない/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "設定を開く" }));
    expect(openSettings).toHaveBeenCalled();
  });

  it("[POP-014][REC-025] 会議が終わったかを聞き、「止める」で止めて保存する。止めるまでの残り時間を表示する", async () => {
    const user = userEvent.setup();
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("対面の打ち合わせ");
    await backend.startRecording(note.id);
    const deadline = Date.now() + 118_000;
    backend.setPopup({ kind: "confirmEnd", app: "Kiroscribe", noteId: note.id, title: note.title, deadline });
    renderWith(backend, <MeetingPopup />);
    const popup = await screen.findByRole("alertdialog", { name: "会議の自動録音" });
    expect(within(popup).getByText("会議は終わりましたか？")).toBeInTheDocument();
    expect(within(popup).getByText("Kiroscribe")).toBeInTheDocument();
    expect(within(popup).getByRole("timer")).toHaveTextContent(/^1:5[78]$/);
    expect(within(popup).getByText(/後に止めて保存します/)).toBeInTheDocument();

    await user.click(within(popup).getByRole("button", { name: "止める" }));
    expect(await within(popup).findByText(/録音を停止しました。「対面の打ち合わせ」に保存しています。/)).toBeInTheDocument();
    expect(backend.recordingCheckAnswers).toEqual([{ noteId: note.id, deadline, keep: false }]);
    expect((await backend.getRecordingStatus()).state).toBe("idle");
  });

  it("[POP-014][REC-025] 「続ける」と答えたら、録音を止めない", async () => {
    const user = userEvent.setup();
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("Zoom 11/20 10:00");
    await backend.startRecording(note.id);
    const deadline = Date.now() + 120_000;
    backend.setPopup({ kind: "confirmEnd", app: "Zoom", noteId: note.id, title: note.title, deadline });
    renderWith(backend, <MeetingPopup />);
    await user.click(await screen.findByRole("button", { name: "続ける" }));
    expect(backend.recordingCheckAnswers).toEqual([{ noteId: note.id, deadline, keep: true }]);
    expect((await backend.getRecordingStatus()).state).toBe("recording");
    // 自動で始めた録音なので、録音中の表示に戻る
    expect(await screen.findByText(/自動で録音・文字起こしを始めました/)).toBeInTheDocument();
    expect(await backend.getPopupState()).toEqual({ kind: "recording", app: "Zoom", noteId: note.id, title: note.title });
  });

  it("[POP-014] 確認を出している録音をほかで止めたら、停止の表示にする（確認の操作は残さない）", async () => {
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("確認中に止める");
    await backend.startRecording(note.id);
    backend.setPopup({ kind: "confirmEnd", app: "Kiroscribe", noteId: note.id, title: note.title, deadline: Date.now() + 60_000 });
    renderWith(backend, <MeetingPopup />);
    const popup = await screen.findByRole("alertdialog", { name: "会議の自動録音" });
    expect(await within(popup).findByText("会議は終わりましたか？")).toBeInTheDocument();

    // メインウィンドウの「停止」（ポップアップの外から録音を止める）
    await act(async () => {
      await backend.stopRecording();
    });
    expect(
      await within(popup).findByText("録音を停止しました。「確認中に止める」に保存しています。"),
    ).toBeInTheDocument();
    expect(within(popup).queryByRole("button", { name: "続ける" })).not.toBeInTheDocument();
  });

  it("[POP-014] 確認に「止める」と答えても、保存が終わるまでは停止の表示にしない", async () => {
    const user = userEvent.setup();
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("保存を待つ");
    await backend.startRecording(note.id);
    backend.setPopup({ kind: "confirmEnd", app: "Kiroscribe", noteId: note.id, title: note.title, deadline: Date.now() + 60_000 });
    let finish!: () => void;
    const saved = new Promise<void>((resolve) => {
      finish = resolve;
    });
    const answer = backend.answerRecordingCheck.bind(backend);
    vi.spyOn(backend, "answerRecordingCheck").mockImplementation(async (...args) => {
      await saved;
      return answer(...args);
    });
    renderWith(backend, <MeetingPopup />);
    const popup = await screen.findByRole("alertdialog", { name: "会議の自動録音" });

    await user.click(await within(popup).findByRole("button", { name: "止める" }));
    expect(await within(popup).findByText("録音を停止しました。文字起こしを仕上げています…")).toBeInTheDocument();
    expect(within(popup).queryByText(/に保存しています/)).not.toBeInTheDocument();

    await act(async () => finish());
    expect(await within(popup).findByText("録音を停止しました。「保存を待つ」に保存しています。")).toBeInTheDocument();
  });

  it("[POP-015] 「止める」の答えが届く前に答えがなく止まったら、その知らせを遅れた答えで上書きしない", async () => {
    const user = userEvent.setup();
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("入れ違い");
    await backend.startRecording(note.id);
    backend.setPopup({ kind: "confirmEnd", app: "Kiroscribe", noteId: note.id, title: note.title, deadline: Date.now() + 1_000 });
    let finish!: () => void;
    const late = new Promise<void>((resolve) => {
      finish = resolve;
    });
    vi.spyOn(backend, "answerRecordingCheck").mockImplementation(async () => {
      await late;
    });
    renderWith(backend, <MeetingPopup />);
    const popup = await screen.findByRole("alertdialog", { name: "会議の自動録音" });

    await user.click(await within(popup).findByRole("button", { name: "止める" }));
    // 本体が、答えを待つ時間が過ぎたので止めた
    act(() => backend.setPopup({ kind: "finishing", app: "Kiroscribe", noteId: note.id, title: note.title }));
    await act(async () => {
      await backend.stopRecording();
    });
    expect(await within(popup).findByText("会議が終わりました。文字起こしを仕上げています…")).toBeInTheDocument();
    act(() => backend.setPopup({ kind: "stoppedUnanswered", app: "Kiroscribe", noteId: note.id, title: note.title }));
    await act(async () => finish());

    expect(
      await within(popup).findByText("答えがなかったため、録音を停止しました。「入れ違い」に保存しています。"),
    ).toBeInTheDocument();
    expect(within(popup).queryByText(/^録音を停止しました。/)).not.toBeInTheDocument();
  });

  it("[POP-015] タイトルが空白だけのノートは、「無題のノート」として知らせる", async () => {
    const backend = await meetingBackend();
    backend.setPopup({ kind: "stoppedUnanswered", app: "Kiroscribe", noteId: "n1", title: "  " });
    renderWith(backend, <MeetingPopup />);
    expect(
      await screen.findByText("答えがなかったため、録音を停止しました。「無題のノート」に保存しています。"),
    ).toBeInTheDocument();
  });

  it("[POP-014] もう出していない確認への答えは、録音に触れない", async () => {
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("古い確認");
    await backend.startRecording(note.id);
    const deadline = Date.now() + 60_000;
    backend.setPopup({ kind: "confirmEnd", app: "Kiroscribe", noteId: note.id, title: note.title, deadline });
    await backend.answerRecordingCheck(note.id, deadline - 300_000, false);
    expect((await backend.getRecordingStatus()).state).toBe("recording");
    expect((await backend.getPopupState())?.kind).toBe("confirmEnd");
  });

  it("[POP-015] 答えがなく止めたことを、自動では閉じずに表示し、ノートを開ける", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      const backend = await meetingBackend();
      backend.setPopup({ kind: "stoppedUnanswered", app: "Kiroscribe", noteId: "n1", title: "週次の定例" });
      const openNote = vi.spyOn(backend, "openNoteFromPopup");
      const close = vi.spyOn(backend, "closePopup");
      renderWith(backend, <MeetingPopup />);
      expect(
        await screen.findByText("答えがなかったため、録音を停止しました。「週次の定例」に保存しています。"),
      ).toBeInTheDocument();
      await act(async () => {
        vi.advanceTimersByTime(60_000);
      });
      expect(close).not.toHaveBeenCalled();
      await user.click(screen.getByRole("button", { name: "ノートを開く" }));
      expect(openNote).toHaveBeenCalledWith("n1", false);
      await user.click(screen.getByRole("button", { name: "議事録を作成" }));
      expect(openNote).toHaveBeenCalledWith("n1", true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("[POP-010] 閉じても録音は止めない", async () => {
    const user = userEvent.setup();
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("Zoom");
    await backend.startRecording(note.id);
    backend.setPopup({ kind: "recording", app: "Zoom", noteId: note.id, title: "Zoom" });
    renderWith(backend, <MeetingPopup />);
    await user.click(await screen.findByRole("button", { name: "閉じる" }));
    expect(backend.popupClosed).toBe(true);
    expect((await backend.getRecordingStatus()).state).toBe("recording");
  });
});

describe("メイン画面との連携", () => {
  it("[MTG-011][MIN-018][POP-007][POP-008] 自動検出で作られたノートが一覧に加わり、ポップアップから開いて議事録を作れる", async () => {
    const backend = await meetingBackend();
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");

    let note!: ReturnType<MemoryBackend["simulateMeetingNote"]>;
    act(() => {
      note = backend.simulateMeetingNote("Google Meet 11/20 15:04");
    });
    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    expect(await within(list).findByText("Google Meet 11/20 15:04")).toBeInTheDocument();

    // 文字起こしがあるノートを「議事録を作成」で開く
    act(() => {
      backend.emitSegment(note.id, "them", "リリースは12月5日で確定です");
      backend.requestOpenNote({ noteId: note.id, enhance: true });
    });
    expect(await screen.findByDisplayValue(/Google Meet 11\/20 15:04|プロダクト定例（リリース確認）/)).toBeInTheDocument();
    await screen.findByRole("button", { name: "作り直す" }, { timeout: 3000 });
    expect((await backend.getNote(note.id)).enhancedNotes).toContain("## 決定事項");
  });
});

describe("設定", () => {
  it("[SET-012] 自動検出と自動起動を切り替えて保存できる", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend();
    renderWith(backend, <SettingsDialog onClose={() => {}} />);
    const auto = await screen.findByLabelText(/会議を検出したら自動で録音/);
    const login = screen.getByLabelText(/Windows の起動時に Kiroscribe を起動/);
    expect(auto).toBeChecked();
    expect(login).toBeChecked();
    await user.click(auto);
    await user.click(login);
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(async () => {
      const saved = await backend.getSettings();
      expect(saved.autoRecordMeetings).toBe(false);
      expect(saved.launchAtLogin).toBe(false);
    });
  });
});

describe("トレイのメニューでの切り替えと、開いている設定画面", () => {
  it("[TRY-004] 設定画面を開いている間にトレイで自動検出を切り替えると、表示も切り替わり、保存しても元に戻らない", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend();
    renderWith(backend, <SettingsDialog onClose={() => {}} />);
    const auto = await screen.findByLabelText(/会議を検出したら自動で録音/);
    expect(auto).toBeChecked();
    // 設定画面で、ほかの項目を書きかけている
    await user.type(screen.getByLabelText("議事録での自分の呼び名"), "田中");
    await user.click(screen.getByLabelText(/相手の声（PC から流れる音声）も取り込む/));

    // トレイのメニューで「会議を検出したら自動で録音」を切り替える
    await act(async () => {
      await backend.toggleAutoRecordFromTray();
    });
    expect((await backend.getSettings()).autoRecordMeetings).toBe(false);
    await waitFor(() => expect(auto).not.toBeChecked());
    // 書きかけの項目は、そのまま残る
    expect(screen.getByLabelText("議事録での自分の呼び名")).toHaveValue("田中");
    expect(screen.getByLabelText(/相手の声（PC から流れる音声）も取り込む/)).not.toBeChecked();

    // そのまま「設定を保存」を押しても、トレイでの切り替えは元に戻らない
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await screen.findByText("設定を保存しました");
    const saved = await backend.getSettings();
    expect(saved.autoRecordMeetings).toBe(false);
    expect(saved.myName).toBe("田中");
    expect(saved.captureSystemAudio).toBe(false);

    // 設定画面で切り替えたあとにトレイで同じ値になっても、表示は食い違わない
    await act(async () => {
      await backend.toggleAutoRecordFromTray();
    });
    await waitFor(() => expect(auto).toBeChecked());
    expect((await backend.getSettings()).autoRecordMeetings).toBe(true);
  });
});

describe("会議検出のポップアップ: 表示が変わるとき・閉じるとき", () => {
  /** ポップアップを描画し、最初の内容の読み込みを待つ（偽のタイマーを使うテスト用） */
  async function renderPopup(backend: Backend) {
    renderWith(backend, <MeetingPopup />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
  }

  it("[POP-004] 利用者が停止を押したときの仕上げ中は「録音を停止しました。文字起こしを仕上げています…」と表示する", async () => {
    const user = userEvent.setup();
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("Google Meet 11/20 15:04");
    await backend.startRecording(note.id);
    backend.setPopup({ kind: "recording", app: "Google Meet", noteId: note.id, title: note.title });
    // 残りの文字起こしを待っている状態で止めておく
    let finish!: () => void;
    const stopped = new Promise<void>((resolve) => {
      finish = resolve;
    });
    const stop = backend.stopRecording.bind(backend);
    vi.spyOn(backend, "stopRecording").mockImplementation(async () => {
      await stopped;
      return stop();
    });
    renderWith(backend, <MeetingPopup />);
    const popup = await screen.findByRole("alertdialog", { name: "会議の自動録音" });

    await user.click(await within(popup).findByRole("button", { name: "停止" }));

    expect(await within(popup).findByText("録音を停止しました。文字起こしを仕上げています…")).toBeInTheDocument();
    expect(within(popup).queryByText(/会議が終わりました/)).not.toBeInTheDocument();
    expect(within(popup).queryByRole("button", { name: "停止" })).not.toBeInTheDocument();

    await act(async () => finish());
    expect(
      await within(popup).findByText("録音を停止しました。「Google Meet 11/20 15:04」に保存しています。"),
    ).toBeInTheDocument();
  });

  it("[POP-004] 会議が終わって自動で止まるときの仕上げ中は「会議が終わりました。文字起こしを仕上げています…」と表示する", async () => {
    const backend = await meetingBackend();
    backend.setPopup({ kind: "recording", app: "Zoom", noteId: "n1", title: "Zoom 11/20 10:00" });
    renderWith(backend, <MeetingPopup />);
    await screen.findByText(/自動で録音・文字起こしを始めました/);

    act(() => backend.setPopup({ kind: "finishing", app: "Zoom", noteId: "n1", title: "Zoom 11/20 10:00" }));

    expect(await screen.findByText("会議が終わりました。文字起こしを仕上げています…")).toBeInTheDocument();
    expect(screen.queryByText("録音を停止しました。文字起こしを仕上げています…")).not.toBeInTheDocument();
  });

  it("[POP-005] 停止の表示は、30 秒たつと自動で閉じる", async () => {
    vi.useFakeTimers();
    try {
      const backend = createMemoryBackend();
      backend.setPopup({ kind: "stopped", app: "Zoom", noteId: "n1", title: "Zoom 11/20 10:00" });
      await renderPopup(backend);
      expect(screen.getByText("録音を停止しました。「Zoom 11/20 10:00」に保存しています。")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "ノートを開く" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "議事録を作成" })).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(29_000);
      });
      expect(backend.popupClosed).toBe(false);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1_000);
      });
      expect(backend.popupClosed).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("[POP-011] 録音中の表示とエラーの表示は、自動では閉じない", async () => {
    vi.useFakeTimers();
    try {
      const payloads = [
        { kind: "recording", app: "Zoom", noteId: "n1", title: "Zoom 11/20 10:00" },
        { kind: "error", app: "Zoom", message: "マイクが見つかりません" },
      ] as const;
      for (const payload of payloads) {
        const backend = createMemoryBackend();
        backend.setPopup(payload);
        await renderPopup(backend);
        await act(async () => {
          await vi.advanceTimersByTimeAsync(10 * 60_000);
        });
        expect(backend.popupClosed).toBe(false);
        expect(screen.getByRole("alertdialog", { name: "会議の自動録音" })).toBeInTheDocument();
        cleanup();
      }
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("メイン画面との連携: ポップアップからの操作", () => {
  it("[POP-011] 自動で始めた録音をメインウィンドウで止めると、ポップアップも停止の表示に変わる", async () => {
    const backend = await meetingBackend();
    const note = backend.simulateMeetingNote("Google Meet 11/20 15:04");
    await backend.startRecording(note.id);
    backend.setPopup({ kind: "recording", app: "Google Meet", noteId: note.id, title: note.title });
    renderWith(backend, <MeetingPopup />);
    const popup = await screen.findByRole("alertdialog", { name: "会議の自動録音" });
    expect(await within(popup).findByRole("timer")).toBeInTheDocument();

    // メインウィンドウの「停止」（ポップアップの外から録音を止める）
    await act(async () => {
      await backend.stopRecording();
    });

    expect(
      await within(popup).findByText("録音を停止しました。「Google Meet 11/20 15:04」に保存しています。"),
    ).toBeInTheDocument();
    expect(within(popup).queryByRole("button", { name: "停止" })).not.toBeInTheDocument();
    expect(within(popup).queryByText(/会議が終わりました/)).not.toBeInTheDocument();
    expect(within(popup).getByRole("button", { name: "議事録を作成" })).toBeInTheDocument();
  });

  it("[POP-011] 録音中の表示のまま残ったポップアップの「停止」は、別のノートの録音を止めない", async () => {
    const user = userEvent.setup();
    const backend = await meetingBackend();
    const meeting = backend.simulateMeetingNote("Zoom 11/20 10:00");
    const other = await backend.createNote();
    // 会議のノートの録音はもう終わっていて、いまは別のノートを手動で録音している
    await backend.startRecording(other.id);
    backend.setPopup({ kind: "recording", app: "Zoom", noteId: meeting.id, title: meeting.title });
    renderWith(backend, <MeetingPopup />);
    const popup = await screen.findByRole("alertdialog", { name: "会議の自動録音" });

    await user.click(await within(popup).findByRole("button", { name: "停止" }));

    expect(await within(popup).findByText("録音を停止しました。「Zoom 11/20 10:00」に保存しています。")).toBeInTheDocument();
    const status = await backend.getRecordingStatus();
    expect(status.state).toBe("recording");
    expect(status.noteId).toBe(other.id);
  });

  it("[POP-007] 「ノートを開く」で、メインウィンドウがそのノートを開き、検索欄を空に戻す", async () => {
    const user = userEvent.setup();
    const backend = await meetingBackend();
    const enhance = vi.spyOn(backend, "enhanceNote");
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    const search = screen.getByLabelText("ノートを検索");
    await user.type(search, "採用");
    let note!: ReturnType<MemoryBackend["simulateMeetingNote"]>;
    act(() => {
      note = backend.simulateMeetingNote("Zoom 11/20 10:00");
    });

    await act(async () => {
      await backend.openNoteFromPopup(note.id, false);
    });

    expect(await screen.findByDisplayValue("Zoom 11/20 10:00")).toBeInTheDocument();
    expect(search).toHaveValue("");
    expect(backend.popupClosed).toBe(true);
    // 「ノートを開く」だけでは、議事録の作成は始めない
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(enhance).not.toHaveBeenCalled();
  });

  it("[POP-009] 「設定を開く」で、メインウィンドウに設定画面が開く", async () => {
    const backend = await meetingBackend();
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    expect(screen.queryByRole("dialog", { name: "設定" })).not.toBeInTheDocument();

    await act(async () => {
      await backend.openSettingsFromPopup();
    });

    expect(await screen.findByRole("dialog", { name: "設定" })).toBeInTheDocument();
    expect(backend.popupClosed).toBe(true);
  });
});
