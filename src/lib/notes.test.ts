import { describe, expect, it } from "vitest";
import { displayTitle, makePreview, matchesQuery, toSummary } from "./notes";
import type { Note } from "./types";
import previewCases from "../../crates/core/tests/fixtures/preview-cases.json";

describe("makePreview", () => {
  it("[NOTE-007] 見出しより本文の最初の行を優先し、記号を外す", () => {
    expect(makePreview("\n\n## 予算\n- 来期")).toBe("来期");
    expect(makePreview("## 見出しだけ")).toBe("見出しだけ");
    expect(makePreview("## ToDo\n- [ ] 見積もり")).toBe("見積もり");
    expect(makePreview("- * 箇条書き")).toBe("箇条書き");
    expect(makePreview("   \n ")).toBe("");
  });

  it("[NOTE-007] 60 文字を超えたら省略記号を付ける（Rust 側と同じ規則）", () => {
    const preview = makePreview("あ".repeat(100));
    expect(Array.from(preview)).toHaveLength(61);
    expect(preview.endsWith("…")).toBe(true);
  });
});

describe("toSummary", () => {
  const base: Note = {
    id: "n1",
    title: "定例",
    userNotes: "- 自分のメモ",
    enhancedNotes: null,
    enhancedAt: null,
    createdAt: 1,
    updatedAt: 2,
  };

  it("[NOTE-007] 議事録があればそちらをプレビューに使う", () => {
    expect(toSummary(base).preview).toBe("自分のメモ");
    expect(toSummary(base).hasEnhanced).toBe(false);
    const enhanced = toSummary({ ...base, enhancedNotes: "## 決定事項\n- 12/5 リリース" });
    expect(enhanced.preview).toBe("12/5 リリース");
    expect(enhanced.hasEnhanced).toBe(true);
  });
});

describe("displayTitle / matchesQuery", () => {
  const summary = {
    id: "1",
    title: "",
    preview: "Budget review",
    hasEnhanced: false,
    createdAt: 0,
    updatedAt: 0,
  };

  it("[NOTE-006][NOTE-008] 空のタイトルは無題として扱い、検索対象にも含める", () => {
    expect(displayTitle("  ")).toBe("無題のノート");
    expect(matchesQuery(summary, "無題")).toBe(true);
    expect(matchesQuery(summary, "budget")).toBe(true);
    expect(matchesQuery(summary, "予算")).toBe(false);
    expect(matchesQuery(summary, "  ")).toBe(true);
  });
});

describe("makePreview（本体と共通の入力例）", () => {
  // 本体側（crates/core/tests/storage.rs）と同じ入力例。片方の規則だけを変えると、どちらかのテストが落ちる
  it("[NOTE-007] 本体と同じ入力から、同じプレビューを作る", () => {
    expect(previewCases.length).toBeGreaterThanOrEqual(10);
    for (const { text, preview } of previewCases) {
      expect(makePreview(text), JSON.stringify(text)).toBe(preview);
    }
  });

  // チェックボックスの記号が 2 つ続く行だけ、本体と結果が違う（本体は続く分をすべて除き、画面は 1 つだけ除く）。
  // いまの振る舞いを固定する。そろえるときは、共通の入力例（preview-cases.json）へ移す
  it("[NOTE-007] チェックボックスの記号は 1 つだけ除く", () => {
    expect(makePreview("- [ ] [ ] 二重")).toBe("[ ] 二重");
    expect(makePreview("- [ ] [x] 二重")).toBe("[x] 二重");
  });
});
