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
