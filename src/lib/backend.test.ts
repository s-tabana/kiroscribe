import { beforeEach, describe, expect, it, vi } from "vitest";

// Tauri の API を差し替えて、画面が本体と OS のダイアログに何を渡すかを確かめる
const tauri = vi.hoisted(() => ({ invoke: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke, isTauri: () => true }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(), save: tauri.save }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({ writeText: vi.fn() }));

import { createTauriBackend, errorMessage } from "./backend";

const OPTIONS = { minutes: true, notes: false, transcript: true };

describe("ファイルへの保存（Tauri 上）", () => {
  beforeEach(() => {
    tauri.invoke.mockReset().mockResolvedValue(undefined);
    tauri.save.mockReset();
  });

  it("[SHR-009] 保存先のダイアログで選べる種類は「Markdown（.md）」と「テキスト（.txt）」", async () => {
    tauri.save.mockResolvedValue("C:\\Users\\me\\Documents\\定例.txt");

    const saved = await createTauriBackend().saveMarkdown("note-1", OPTIONS, "2026-11-20 定例.md");

    expect(tauri.save).toHaveBeenCalledExactlyOnceWith({
      defaultPath: "2026-11-20 定例.md",
      filters: [
        { name: "Markdown", extensions: ["md"] },
        { name: "テキスト", extensions: ["txt"] },
      ],
    });
    // 選んだ保存先を、そのまま本体に渡す
    expect(tauri.invoke).toHaveBeenCalledExactlyOnceWith("save_markdown", {
      noteId: "note-1",
      options: OPTIONS,
      path: "C:\\Users\\me\\Documents\\定例.txt",
    });
    expect(saved).toBe("C:\\Users\\me\\Documents\\定例.txt");
  });

  it("[SHR-008] 保存先のダイアログを取り消したら、何も書かない", async () => {
    tauri.save.mockResolvedValue(null);

    const saved = await createTauriBackend().saveMarkdown("note-1", OPTIONS, "2026-11-20 定例.md");

    expect(saved).toBeNull();
    expect(tauri.invoke).not.toHaveBeenCalled();
  });
});

describe("errorMessage", () => {
  it("[ERR-011] 本体のエラー（文字列）はそのまま、理由の分からない失敗は「不明なエラーが発生しました」にする", () => {
    expect(errorMessage("ノートが見つかりません: abc")).toBe("ノートが見つかりません: abc");
    expect(errorMessage(new Error("失敗しました"))).toBe("失敗しました");
    expect(errorMessage({ code: 42 })).toBe("不明なエラーが発生しました");
    expect(errorMessage(undefined)).toBe("不明なエラーが発生しました");
  });
});

describe("デバッグ用のログ（Tauri 上）", () => {
  beforeEach(() => {
    tauri.invoke.mockReset().mockResolvedValue(undefined);
    tauri.save.mockReset();
  });

  it("[LOG-008] 本体のコマンドが失敗したら、コマンドの名前と理由をデバッグ用のログに書いてから、失敗を返す", async () => {
    tauri.invoke.mockImplementation(async (command: string) => {
      if (command === "start_recording") throw "マイクが見つかりません";
      return undefined;
    });

    await expect(createTauriBackend().startRecording("note-1")).rejects.toBe("マイクが見つかりません");

    // 引数（ノートの ID など）は書かない
    expect(tauri.invoke).toHaveBeenLastCalledWith("write_debug_log", {
      level: "error",
      event: "ui.command_failed",
      fields: { command: "start_recording", message: "マイクが見つかりません" },
    });
  });

  it("[LOG-008] ログを書くこと自体が失敗しても、失敗にせず、ログを書き直そうとしない", async () => {
    tauri.invoke.mockRejectedValue("書けません");

    await expect(createTauriBackend().writeDebugLog("info", "ui.loaded", { view: "main" })).resolves.toBeUndefined();

    expect(tauri.invoke).toHaveBeenCalledOnce();
  });

  it("[LOG-011] 保存先のダイアログで選べる種類は ZIP（.zip）だけで、選んだ場所を本体に渡す。取り消したら何もしない", async () => {
    tauri.save.mockResolvedValue("C:\\Users\\me\\Desktop\\debug.zip");

    const saved = await createTauriBackend().saveDebugBundle("kiroscribe-debug-20261127-1904.zip");

    expect(tauri.save).toHaveBeenCalledExactlyOnceWith({
      defaultPath: "kiroscribe-debug-20261127-1904.zip",
      filters: [{ name: "ZIP", extensions: ["zip"] }],
    });
    expect(tauri.invoke).toHaveBeenCalledExactlyOnceWith("save_debug_bundle", { path: "C:\\Users\\me\\Desktop\\debug.zip" });
    expect(saved).toBe("C:\\Users\\me\\Desktop\\debug.zip");

    tauri.invoke.mockClear();
    tauri.save.mockResolvedValue(null);
    expect(await createTauriBackend().saveDebugBundle("x.zip")).toBeNull();
    expect(tauri.invoke).not.toHaveBeenCalled();
  });
});
