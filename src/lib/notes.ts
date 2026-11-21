import type { Note, NoteSummary } from "./types";

const stripMarkers = (line: string) =>
  line
    .replace(/^[#\-*・>\s]+/, "")
    .replace(/^\[[ x]\] /, "")
    .trimEnd();

/** Rust 側 make_preview と同じ規則: 見出しより本文の最初の行を優先し、記号を外す */
export function makePreview(text: string): string {
  const lines = text.split("\n").filter((l) => l.trim().length > 0);
  const chosen = lines.find((l) => !l.trimStart().startsWith("#")) ?? lines[0] ?? "";
  const line = stripMarkers(chosen);
  const chars = Array.from(line);
  return chars.length > 60 ? chars.slice(0, 60).join("") + "…" : line;
}

export function toSummary(note: Note): NoteSummary {
  const source = note.enhancedNotes?.trim() ? note.enhancedNotes : note.userNotes;
  return {
    id: note.id,
    title: note.title,
    preview: makePreview(source),
    hasEnhanced: note.enhancedNotes !== null,
    createdAt: note.createdAt,
    updatedAt: note.updatedAt,
  };
}

export const UNTITLED = "無題のノート";

export function displayTitle(title: string): string {
  return title.trim() || UNTITLED;
}

/** サイドバーの検索。タイトルとプレビューの部分一致（大文字小文字を区別しない） */
export function matchesQuery(note: NoteSummary, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return (
    displayTitle(note.title).toLowerCase().includes(q) || note.preview.toLowerCase().includes(q)
  );
}
