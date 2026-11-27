import { afterEach, describe, expect, it } from "vitest";
import { installDebugLogging } from "./debugLog";
import { createMemoryBackend } from "./memoryBackend";

describe("installDebugLogging", () => {
  let uninstall: (() => void) | null = null;
  afterEach(() => uninstall?.());

  it("[LOG-008] 画面を開いたことと、捕まえられなかったエラーを、本体のデバッグ用のログに書く", async () => {
    const backend = createMemoryBackend();
    uninstall = installDebugLogging(backend, window, "main");

    window.dispatchEvent(
      new ErrorEvent("error", {
        message: "Uncaught TypeError: x is undefined",
        error: new TypeError("x is undefined"),
        filename: "tauri://localhost/assets/index-abc123.js?v=1",
        lineno: 42,
      }),
    );
    const rejection = new Event("unhandledrejection") as PromiseRejectionEvent;
    Object.defineProperty(rejection, "reason", { value: "ノートを保存できませんでした" });
    window.dispatchEvent(rejection);

    expect(backend.debugLog).toEqual([
      { level: "info", event: "ui.loaded", fields: { view: "main" } },
      {
        level: "error",
        event: "ui.error",
        fields: { view: "main", message: "x is undefined", file: "index-abc123.js", line: 42 },
      },
      {
        level: "error",
        event: "ui.unhandled_rejection",
        fields: { view: "main", message: "ノートを保存できませんでした" },
      },
    ]);

    uninstall();
    uninstall = null;
    window.dispatchEvent(new ErrorEvent("error", { message: "after" }));
    expect(backend.debugLog).toHaveLength(3);
  });
});
