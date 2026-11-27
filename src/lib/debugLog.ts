import type { Backend } from "./backend";

/** 画面のどこかで捕まえられなかったエラーの文（Error ならその文、それ以外は文字列にしたもの） */
function describe(reason: unknown): string {
  if (reason instanceof Error) return reason.message;
  if (typeof reason === "string") return reason;
  return "不明なエラー";
}

/** スクリプトの場所から、ファイルの名前だけを取り出す（URL のほかの部分は書かない） */
function fileName(source: string | undefined): string | undefined {
  if (!source) return undefined;
  return source.split(/[\\/]/).pop()?.split("?")[0];
}

/**
 * 画面の出来事をデバッグ用のログに書く: 画面を開いたことと、捕まえられなかったエラー。
 * 書くのはエラーの文と、起きたファイルの名前・行だけ（入力中の文やノートの内容は書かない）。
 * 戻り値を呼ぶと、書くのをやめる。
 */
export function installDebugLogging(backend: Backend, target: Window, view: "main" | "popup"): () => void {
  const write = (level: "info" | "error", event: string, fields?: Record<string, unknown>) => {
    void backend.writeDebugLog(level, event, fields).catch(() => undefined);
  };
  const onError = (event: ErrorEvent) => {
    write("error", "ui.error", {
      view,
      message: describe(event.error ?? event.message),
      file: fileName(event.filename),
      line: event.lineno,
    });
  };
  const onRejection = (event: PromiseRejectionEvent) => {
    write("error", "ui.unhandled_rejection", { view, message: describe(event.reason) });
  };
  target.addEventListener("error", onError);
  target.addEventListener("unhandledrejection", onRejection);
  write("info", "ui.loaded", { view });
  return () => {
    target.removeEventListener("error", onError);
    target.removeEventListener("unhandledrejection", onRejection);
  };
}
