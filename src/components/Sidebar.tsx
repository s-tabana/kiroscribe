import { useMemo } from "react";
import type { NoteSummary } from "../lib/types";
import { groupByDay, timeLabel } from "../lib/dates";
import { displayTitle, matchesQuery } from "../lib/notes";
import type { Updater } from "../hooks/useUpdater";
import { UpdateBanner, UpdatedNotice } from "./UpdateBanner";
import { GearIcon, HelpIcon, LogoMark, PlusIcon, SearchIcon } from "./icons";

interface SidebarProps {
  notes: NoteSummary[];
  selectedId: string | null;
  /** 録音中（または仕上げ中）のノート */
  recordingNoteId: string | null;
  query: string;
  /** メモの本文・議事録・文字起こしまで含めた検索で当てはまったノート（結果がまだなければ null） */
  foundIds: ReadonlySet<string> | null;
  isPreview: boolean;
  /** アプリ内更新の状態（新しいバージョンがあれば下部に知らせを出す） */
  updater: Updater;
  /** 自動で更新した直後なら、更新後のバージョン（下部に知らせを出す） */
  updatedVersion: string | null;
  onDismissUpdated(): void;
  onOpenReleaseNotes(): void;
  onQueryChange(query: string): void;
  onSelect(id: string): void;
  onCreate(): void;
  onOpenSettings(): void;
  onOpenHelp(): void;
}

export function Sidebar(props: SidebarProps) {
  const { notes, selectedId, query, foundIds } = props;
  const groups = useMemo(
    () => groupByDay(notes.filter((n) => matchesQuery(n, query) || (foundIds?.has(n.id) ?? false))),
    [notes, query, foundIds],
  );

  return (
    <aside className="sidebar">
      <div className="sidebar-brand">
        <LogoMark />
        <span className="brand-name">Kiroscribe</span>
        <button
          type="button"
          className="icon-button"
          onClick={props.onOpenHelp}
          aria-label="ヘルプ"
          title="ヘルプ（使い方・リリースノート）"
        >
          <HelpIcon />
        </button>
        <button
          type="button"
          className="icon-button"
          onClick={props.onOpenSettings}
          aria-label="設定"
          title="設定"
        >
          <GearIcon />
        </button>
      </div>

      <button type="button" className="button button-primary new-note" onClick={props.onCreate}>
        <PlusIcon size={16} />
        新しいノート
        <kbd>Ctrl+N</kbd>
      </button>

      <label className="search">
        <SearchIcon size={15} />
        <input
          type="search"
          placeholder="ノートを検索"
          value={query}
          onChange={(e) => props.onQueryChange(e.target.value)}
          aria-label="ノートを検索"
        />
      </label>

      <nav className="note-list" aria-label="ノート一覧">
        {groups.length === 0 && (
          <p className="note-list-empty">
            {notes.length === 0 ? "まだノートがありません" : "一致するノートがありません"}
          </p>
        )}
        {groups.map((group) => (
          <section key={group.label} className="note-group">
            <h2 className="note-group-label">{group.label}</h2>
            <ul>
              {group.items.map((note) => (
                <li key={note.id}>
                  <button
                    type="button"
                    className="note-item"
                    aria-current={note.id === selectedId ? "page" : undefined}
                    onClick={() => props.onSelect(note.id)}
                  >
                    <span className="note-item-title">
                      <span className={note.title.trim() ? "" : "is-untitled"}>
                        {displayTitle(note.title)}
                      </span>
                      {note.id === props.recordingNoteId ? (
                        <span className="rec-dot is-live" title="録音中" aria-label="録音中" />
                      ) : (
                        <time>{timeLabel(note.createdAt)}</time>
                      )}
                    </span>
                    {note.preview && <span className="note-item-preview">{note.preview}</span>}
                  </button>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </nav>

      {props.updatedVersion && (
        <UpdatedNotice
          version={props.updatedVersion}
          onOpenReleaseNotes={props.onOpenReleaseNotes}
          onDismiss={props.onDismissUpdated}
        />
      )}
      <UpdateBanner updater={props.updater} />

      {props.isPreview && (
        <p className="preview-banner">ブラウザプレビュー：データは保存されません</p>
      )}
    </aside>
  );
}
