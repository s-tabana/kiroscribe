import { describe, expect, it } from "vitest";
import { MAX_EXTRA_BROWSERS, NOT_BROWSERS, formatBrowsers, normalizeBrowsers, parseBrowsers } from "./browsers";

describe("判定に足すブラウザ", () => {
  // 本体（crates/core/src/settings.rs の extra_browsers_are_saved_as_exe_names）と同じ例で、同じ結果になる
  it("[SET-021] 実行ファイルの名前（小文字、.exe 付き）にそろえ、空・重複・Kiroscribe 自身を除く", () => {
    const names = [
      " MyBrowser.EXE ",
      '"C:\\Program Files\\Sample\\sample.exe"',
      "other",
      "mybrowser.exe",
      "",
      "Kiroscribe.exe",
      "D:/Tools/third.exe",
      "C:\\Program Files (x86)\\Microsoft\\EdgeWebView\\Application\\msedgewebview2.exe",
      "Update.exe",
      "ms-teamsupdate",
      "msteams_autostarter.exe",
      "ZCSCPTHOST.EXE",
    ];
    expect(normalizeBrowsers(names)).toEqual(["mybrowser.exe", "sample.exe", "other.exe", "third.exe"]);
    expect(normalizeBrowsers(Array.from({ length: 30 }, (_, i) => `b${i}.exe`))).toHaveLength(MAX_EXTRA_BROWSERS);
    expect(normalizeBrowsers(["a".repeat(80)])).toEqual([]);
  });

  it("[SET-021][MTG-003] MTG-003 で対象にしないプロセスは足せない", () => {
    expect(normalizeBrowsers(NOT_BROWSERS.map((name) => name.toUpperCase()))).toEqual([]);
    expect(normalizeBrowsers(["MSEdgeWebView2", "update", "mine"])).toEqual(["mine.exe"]);
  });

  it("[SET-021] 入力欄の文字は「,」「、」か改行で区切る", () => {
    expect(parseBrowsers(" a.exe, b、c\nd ,, ")).toEqual(["a.exe", "b", "c", "d"]);
    expect(parseBrowsers("")).toEqual([]);
    expect(formatBrowsers(["a.exe", "b.exe"])).toBe("a.exe, b.exe");
  });
});
