// 会議の検出の対象に足すブラウザ（SET-021）。整え方は本体（crates/core/src/settings.rs の normalize_browsers）と同じ。

/** 足せるブラウザの数の上限 */
export const MAX_EXTRA_BROWSERS = 20;
/** 名前 1 つの長さの上限（文字数） */
const MAX_BROWSER_NAME_CHARS = 64;

/** 入力欄の文字（「,」「、」か改行で区切る）を、ブラウザの名前の並びにする。 */
export function parseBrowsers(text: string): string[] {
  return text
    .split(/[,、，\n]/)
    .map((name) => name.trim())
    .filter((name) => name !== "");
}

/** 入力欄に出す形（「, 」でつなぐ）。 */
export function formatBrowsers(names: string[]): string {
  return names.join(", ");
}

/**
 * 実行ファイルの名前にそろえる: 前後の空白・引用符とフォルダの部分を除き、小文字にし、末尾が .exe でなければ .exe を付ける。
 * 空の名前・長すぎる名前・Kiroscribe 自身・重複は除き、20 個までにする。
 */
export function normalizeBrowsers(names: string[]): string[] {
  const out: string[] = [];
  for (const raw of names) {
    const unquoted = raw.trim().replace(/^"+|"+$/g, "");
    let name = (unquoted.split(/[\\/]/).pop() ?? "").trim().toLowerCase();
    if (name === "" || name === ".exe") continue;
    if (!name.endsWith(".exe")) name = `${name}.exe`;
    if (Array.from(name).length > MAX_BROWSER_NAME_CHARS || name === "kiroscribe.exe" || out.includes(name)) continue;
    out.push(name);
    if (out.length === MAX_EXTRA_BROWSERS) break;
  }
  return out;
}
