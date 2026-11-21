import { describe, expect, it } from "vitest";
import { classifyLine, classifyLines, normalize, similarity, stripTitle } from "./attribution";

describe("normalize", () => {
  it("Markdown の記号・空白・句読点を除き、全角半角をそろえる", () => {
    expect(normalize("- [ ] 田花: 見積もりを出す（来週）")).toBe("田花見積もりを出す来週");
    expect(normalize("## ＱＡ の人手")).toBe("qaの人手");
    expect(normalize("1. リリース日 12/5")).toBe("リリース日125");
  });
});

describe("classifyLine", () => {
  const notes = ["- リリース日 12/5 で確定？", "- QA の人手が足りない", "- 価格改定は来期"].map(normalize);

  it("メモをほぼそのまま残した行は自分由来", () => {
    expect(classifyLine("- リリース日は 12/5 で確定", notes)).toBe("me");
    expect(classifyLine("- QA の人手が足りないため、外部に依頼する", notes)).toBe("me");
    expect(classifyLine("- 価格改定は来期に持ち越し", notes)).toBe("me");
  });

  it("メモにない内容は AI が補った行", () => {
    expect(classifyLine("- 外部 API の回答待ちの 1 件は次回リリースに回す", notes)).toBe("ai");
    expect(classifyLine("- [ ] 相手: 営業へ価格の件を共有（今週中）", notes)).toBe("ai");
  });

  it("見出しや短すぎる行は色分けしない", () => {
    expect(classifyLine("## 決定事項", notes)).toBe("neutral");
    expect(classifyLine("- ok", [])).toBe("ai");
    expect(classifyLine("- a", notes)).toBe("neutral");
  });

  it("短いメモ（2〜3 文字）が含まれるだけでは自分由来にしない", () => {
    const short = ["QA"].map(normalize);
    expect(classifyLine("- QA チームから品質基準の見直し提案があった", short)).toBe("ai");
  });
});

describe("classifyLines", () => {
  it("行番号ごとに由来を返し、空行は含めない", () => {
    const minutes = "## 決定事項\n- リリース日は 12/5 で確定\n\n- 営業へ共有する";
    const result = classifyLines(minutes, "- リリース日 12/5 で確定？");
    expect([...result.entries()]).toEqual([
      [1, "neutral"],
      [2, "me"],
      [4, "ai"],
    ]);
  });
});

describe("similarity / stripTitle", () => {
  it("同じ文字列は 1、無関係なら 0", () => {
    expect(similarity("あいうえ", "あいうえ")).toBe(1);
    expect(similarity("あいう", "かきく")).toBe(0);
  });

  it("先頭のタイトル行だけを外す", () => {
    expect(stripTitle("# 定例\n\n## 決定事項")).toBe("## 決定事項");
    expect(stripTitle("# 生成中")).toBe("");
    expect(stripTitle("## 決定事項")).toBe("## 決定事項");
  });
});
