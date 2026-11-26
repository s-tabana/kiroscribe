// 会議の相棒の設定の欄に出す、費用の目安と索引の状態の文。

import type { SearchIndexStatus } from "./types";

/**
 * 相棒のモデルの単価（100 万トークンあたりの米ドル。入力・出力）。
 * Anthropic の料金表から写した日: 2026-11-03
 */
export const COMPANION_PRICES: Record<string, { input: number; output: number }> = {
  "claude-opus-5-5": { input: 4, output: 20 },
  "claude-sonnet-5-5": { input: 2, output: 10 },
  "claude-haiku-5-5": { input: 0.1, output: 0.5 },
  "claude-fable-5-2": { input: 8, output: 40 },
};

/** 1 時間の会議の呼び出しの目安の上限側（回数と、1 回の入力・出力のトークン数） */
export const HOURLY_CALLS = {
  judge: { calls: 60, input: 1_500, output: 200 },
  select: { calls: 15, input: 4_500, output: 400 },
};

/** 1 時間の会議の費用の目安（米ドル。キャッシュによる割引は含めない）。単価を持たないモデルなら null */
export function hourlyCostUsd(model: string): number | null {
  const price = COMPANION_PRICES[model];
  if (!price) return null;
  const tokens = Object.values(HOURLY_CALLS).reduce(
    (sum, call) => ({ input: sum.input + call.calls * call.input, output: sum.output + call.calls * call.output }),
    { input: 0, output: 0 },
  );
  return (tokens.input * price.input + tokens.output * price.output) / 1_000_000;
}

/** 費用の表示（1 セント単位に切り上げる） */
export function hourlyCostText(model: string): string {
  const usd = hourlyCostUsd(model);
  if (usd === null) return "このモデルの料金は、Anthropic の料金表で確かめてください";
  const cents = Math.ceil(usd * 100 - 1e-9);
  return `1 時間の会議で最大 約 ${(cents / 100).toFixed(2)} ドル`;
}

const count = (n: number) => n.toLocaleString("ja-JP");

/** 索引の状態の文 */
export function indexStatusText(status: SearchIndexStatus): string {
  switch (status.state) {
    case "building":
      return status.total > 0 ? `索引を作っています（${count(status.done)} / ${count(status.total)} 件）` : "索引を作っています";
    case "ready": {
      if (status.updatedAt === null) return `索引に入っています: ${count(status.files)} 件`;
      const at = new Date(status.updatedAt);
      const time = `${at.getHours()}:${String(at.getMinutes()).padStart(2, "0")}`;
      return `索引に入っています: ${count(status.files)} 件（${time} に更新）`;
    }
    case "off":
      return "会議の相棒がオフのため、索引は作っていません";
  }
}
