/**
 * 議事録の各行が「自分のメモ由来」か「AI が補ったもの」かを判定する（議事録の色分け用）。
 *
 * AI の出力に印を付けさせる方法は守られないことがあるため、メモと議事録の文字の重なりで機械的に判定する。
 * 文字の 2-gram の Dice 係数と、ある程度長いメモが含まれているかで判断する。
 */

export type LineSource = "me" | "ai" | "neutral";

const MARKDOWN_PREFIX = /^\s*(#{1,6}\s+|[-*+]\s+(\[[ xX]\]\s+)?|\d+[.)]\s+|>\s*)*/;
const NOISE = /[\s\p{P}\p{S}]/gu;

export function normalize(line: string): string {
  return line.normalize("NFKC").toLowerCase().replace(MARKDOWN_PREFIX, "").replace(NOISE, "");
}

function bigrams(text: string): Set<string> {
  const chars = Array.from(text);
  const grams = new Set<string>();
  for (let i = 0; i < chars.length - 1; i++) grams.add(chars[i] + chars[i + 1]);
  if (chars.length === 1) grams.add(chars[0]);
  return grams;
}

export function similarity(a: string, b: string): number {
  const ga = bigrams(a);
  const gb = bigrams(b);
  if (ga.size === 0 || gb.size === 0) return 0;
  let shared = 0;
  ga.forEach((g) => gb.has(g) && shared++);
  return (2 * shared) / (ga.size + gb.size);
}

const THRESHOLD = 0.5;
const MIN_CONTAIN = 4;

/** 1 行の由来。見出しや空行は neutral */
export function classifyLine(line: string, noteLines: string[]): LineSource {
  if (/^\s*#{1,6}\s/.test(line)) return "neutral";
  const norm = normalize(line);
  if (Array.from(norm).length < 2) return "neutral";
  for (const note of noteLines) {
    if (note.length >= MIN_CONTAIN && norm.includes(note)) return "me";
    if (norm.length >= MIN_CONTAIN && note.includes(norm)) return "me";
    if (similarity(norm, note) >= THRESHOLD) return "me";
  }
  return "ai";
}

/** 議事録の行番号（1 始まり）→ 由来 */
export function classifyLines(markdown: string, userNotes: string): Map<number, LineSource> {
  const noteLines = userNotes
    .split("\n")
    .map(normalize)
    .filter((l) => Array.from(l).length >= 2);
  const result = new Map<number, LineSource>();
  markdown.split("\n").forEach((line, index) => {
    if (line.trim()) result.set(index + 1, classifyLine(line, noteLines));
  });
  return result;
}

/** 生成結果の先頭の「# タイトル」を外す（Rust 側 split_title と同じ扱い。生成中のプレビュー用） */
export function stripTitle(markdown: string): string {
  const trimmed = markdown.trimStart();
  if (!trimmed.startsWith("# ")) return markdown;
  const newline = trimmed.indexOf("\n");
  return newline === -1 ? "" : trimmed.slice(newline + 1).trimStart();
}
