import { describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
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
  it("録音中の表示から停止でき、停止後は議事録の作成へ進める", async () => {
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

  it("会議の終了を知らせる（Rust 側からの更新を反映）", async () => {
    const backend = await meetingBackend();
    backend.setPopup({ kind: "recording", app: "Slack ハドル", noteId: "n1", title: "Slack ハドル 11/20 10:00" });
    renderWith(backend, <MeetingPopup />);
    await screen.findByText(/自動で録音・文字起こしを始めました/);

    act(() => backend.setPopup({ kind: "finishing", app: "Slack ハドル", noteId: "n1", title: "t" }));
    expect(await screen.findByText(/文字起こしを仕上げています/)).toBeInTheDocument();
    act(() => backend.setPopup({ kind: "stopped", app: "Slack ハドル", noteId: "n1", title: "Slack ハドル 11/20 10:00" }));
    expect(await screen.findByText("録音を停止しました。「Slack ハドル 11/20 10:00」に保存しています。")).toBeInTheDocument();
  });

  it("API キーがないときは設定へ案内する", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend();
    backend.setPopup({ kind: "error", app: "Zoom", message: "OpenAI の API キーが設定されていないため、自動で録音できませんでした" });
    const openSettings = vi.spyOn(backend, "openSettingsFromPopup");
    renderWith(backend, <MeetingPopup />);
    expect(await screen.findByText(/API キーが設定されていない/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "設定を開く" }));
    expect(openSettings).toHaveBeenCalled();
  });

  it("閉じても録音は止めない", async () => {
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
  it("自動検出で作られたノートが一覧に加わり、ポップアップから開いて議事録を作れる", async () => {
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
  it("自動検出と自動起動を切り替えて保存できる", async () => {
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
