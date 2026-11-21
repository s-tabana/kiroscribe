import { stripTitle } from "../lib/attribution";
import { Markdown } from "./Markdown";
import { EditIcon, RefreshIcon } from "./icons";

interface MinutesViewProps {
  /** 保存済みの議事録 */
  minutes: string;
  userNotes: string;
  editing: boolean;
  /** 生成中なら途中までの文章 */
  streaming: string | null;
  onEdit(value: string): void;
  onToggleEdit(): void;
  onRegenerate(): void;
  onCancel(): void;
}

/** AI が仕上げた議事録。自分のメモ由来の行は濃く、AI が補った行は薄く表示する */
export function MinutesView(props: MinutesViewProps) {
  if (props.streaming !== null) {
    const preview = stripTitle(props.streaming);
    return (
      <section className="minutes is-streaming" aria-label="議事録" aria-busy="true">
        <div className="minutes-toolbar">
          <span className="generating">
            <span className="spinner" aria-hidden="true" />
            {preview ? "議事録を作成しています…" : "メモと文字起こしを読んでいます…"}
          </span>
          <button type="button" className="button button-ghost" onClick={props.onCancel}>
            中止
          </button>
        </div>
        {preview && <Markdown text={preview} />}
      </section>
    );
  }

  return (
    <section className="minutes" aria-label="議事録" aria-busy="false">
      <div className="minutes-toolbar">
        <span className="legend">
          <span>
            <b className="legend-me">濃い文字</b>＝あなたのメモ由来
          </span>
          <span>
            <b className="legend-ai">薄い文字</b>＝AI が補足
          </span>
        </span>
        <button type="button" className="button button-ghost" onClick={props.onToggleEdit}>
          <EditIcon size={15} />
          {props.editing ? "編集を終える" : "編集"}
        </button>
        <button type="button" className="button button-ghost" onClick={props.onRegenerate}>
          <RefreshIcon size={15} />
          作り直す
        </button>
      </div>
      {props.editing ? (
        <textarea
          className="minutes-editor"
          value={props.minutes}
          aria-label="議事録の編集"
          onChange={(e) => props.onEdit(e.target.value)}
          autoFocus
        />
      ) : (
        <Markdown text={props.minutes} userNotes={props.userNotes} />
      )}
    </section>
  );
}
