import type { RecordingStatus } from "../lib/types";
import { elapsedMs, formatElapsed } from "../lib/elapsed";
import { useNow } from "../hooks/useNow";
import { LevelMeter } from "./LevelMeter";
import type { PanelTab } from "./SidePanel";
import {
  ChatIcon,
  MicIcon,
  PauseIcon,
  PlayIcon,
  SparklesIcon,
  StopIcon,
  TranscriptIcon,
} from "./icons";

interface RecordingBarProps {
  noteId: string;
  status: RecordingStatus;
  busy: boolean;
  segmentCount: number;
  panel: PanelTab | null;
  /** 議事録をまだ作っていないときの「議事録を作成」ボタン。不要なら null */
  enhance: { running: boolean; onClick(): void } | null;
  onStart(): void;
  onPause(): void;
  onResume(): void;
  onStop(): void;
  onTogglePanel(tab: PanelTab): void;
  onOpenRecordingNote(): void;
}

/** ノート下部に浮かぶ録音コントロール */
export function RecordingBar(props: RecordingBarProps) {
  const { status, noteId } = props;
  const isThisNote = status.noteId === noteId;
  const running = status.state === "recording";
  const now = useNow(running && status.noteId !== null);
  const elapsed = formatElapsed(elapsedMs(status, now));

  // 録音中はボタンが多いので、パネルの切り替えはアイコンだけにする
  const compact = isThisNote && (status.state === "recording" || status.state === "paused");
  const panelToggles = (
    <>
      <button
        type="button"
        className={`bar-button ${props.panel === "transcript" ? "is-active" : ""}`}
        onClick={() => props.onTogglePanel("transcript")}
        aria-pressed={props.panel === "transcript"}
        aria-label={compact ? "文字起こし" : undefined}
        title="文字起こしを表示"
      >
        <TranscriptIcon size={16} />
        {!compact && "文字起こし"}
        {props.segmentCount > 0 && <span className="count">{props.segmentCount}</span>}
      </button>
      <button
        type="button"
        className={`bar-button ${props.panel === "chat" ? "is-active" : ""}`}
        onClick={() => props.onTogglePanel("chat")}
        aria-pressed={props.panel === "chat"}
        aria-label={compact ? "AI チャット" : undefined}
        title="この会議について AI に質問"
      >
        <ChatIcon size={15} />
        {!compact && "AI チャット"}
      </button>
    </>
  );

  let content;
  if (status.state !== "idle" && !isThisNote) {
    content = (
      <>
        <span className="rec-dot is-live" aria-hidden="true" />
        <span className="bar-text">別のノートで録音中 {elapsed}</span>
        <button type="button" className="bar-button" onClick={props.onOpenRecordingNote}>
          そのノートを開く
        </button>
      </>
    );
  } else if (status.state === "finishing") {
    content = (
      <>
        <span className="spinner" aria-hidden="true" />
        <span className="bar-text">文字起こしを仕上げています…</span>
        {panelToggles}
      </>
    );
  } else if (status.state === "recording" || status.state === "paused") {
    content = (
      <>
        <span className={`rec-dot ${running ? "is-live" : ""}`} aria-hidden="true" />
        <span className="bar-text" role="timer">
          {running ? "録音中" : "一時停止中"}
          <strong>{elapsed}</strong>
        </span>
        {running && (
          <span className="levels">
            {status.sources.map((s) => (
              <LevelMeter key={s} speaker={s} />
            ))}
          </span>
        )}
        {running ? (
          <button type="button" className="bar-button" onClick={props.onPause} disabled={props.busy}>
            <PauseIcon size={15} />
            一時停止
          </button>
        ) : (
          <button type="button" className="bar-button" onClick={props.onResume} disabled={props.busy}>
            <PlayIcon size={14} />
            再開
          </button>
        )}
        <button type="button" className="bar-button bar-stop" onClick={props.onStop} disabled={props.busy}>
          <StopIcon size={14} />
          停止
        </button>
        {panelToggles}
      </>
    );
  } else {
    content = (
      <>
        <button type="button" className="record-button" onClick={props.onStart} disabled={props.busy}>
          <MicIcon size={16} />
          {props.segmentCount > 0 ? "録音を再開" : "録音を開始"}
        </button>
        {props.enhance && (
          <button
            type="button"
            className="enhance-button"
            onClick={props.enhance.onClick}
            disabled={props.enhance.running}
          >
            <SparklesIcon size={16} />
            {props.enhance.running ? "作成中…" : "議事録を作成"}
          </button>
        )}
        {panelToggles}
      </>
    );
  }

  return (
    <div className={`recording-bar state-${isThisNote ? status.state : "idle"}`} aria-label="録音">
      {content}
    </div>
  );
}
