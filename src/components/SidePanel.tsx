import type { Segment, Speaker, TranscriptBreak } from "../lib/types";
import { ChatPanel } from "./ChatPanel";
import { TranscriptList } from "./TranscriptPanel";
import { CloseIcon } from "./icons";

export type PanelTab = "transcript" | "chat";

interface SidePanelProps {
  noteId: string;
  tab: PanelTab;
  segments: Segment[];
  breaks: TranscriptBreak[];
  live: boolean;
  sources: Speaker[];
  onTabChange(tab: PanelTab): void;
  onClose(): void;
  onError(message: string): void;
}

const TAB_LABEL: Record<PanelTab, string> = { transcript: "文字起こし", chat: "AI チャット" };

/** ノート右側のパネル（文字起こし / AI チャット） */
export function SidePanel(props: SidePanelProps) {
  const { tab } = props;
  return (
    <aside className="side-panel" aria-label={TAB_LABEL[tab]}>
      <header className="panel-header" role="tablist">
        {(Object.keys(TAB_LABEL) as PanelTab[]).map((t) => (
          <button
            key={t}
            type="button"
            role="tab"
            aria-selected={tab === t}
            className={`panel-tab ${tab === t ? "is-active" : ""}`}
            onClick={() => props.onTabChange(t)}
          >
            {TAB_LABEL[t]}
            {t === "transcript" && props.live && <span className="live-badge">ライブ</span>}
          </button>
        ))}
        <button type="button" className="icon-button" onClick={props.onClose} aria-label="パネルを閉じる">
          <CloseIcon size={16} />
        </button>
      </header>
      {tab === "transcript" ? (
        <TranscriptList
          segments={props.segments}
          breaks={props.breaks}
          live={props.live}
          sources={props.sources}
        />
      ) : (
        <ChatPanel noteId={props.noteId} onError={props.onError} />
      )}
    </aside>
  );
}
