import { useEffect, useLayoutEffect, useRef } from "react";
import { SPEAKER_LABEL, type Segment, type Speaker } from "../lib/types";
import { timeLabel } from "../lib/dates";

interface TranscriptListProps {
  segments: Segment[];
  live: boolean;
  sources: Speaker[];
}

/** 話者が同じで間隔の短い発話はひとまとまりに表示する */
export function groupSegments(segments: Segment[], maxGapMs = 60_000): Segment[][] {
  const groups: Segment[][] = [];
  for (const segment of segments) {
    const last = groups[groups.length - 1];
    const prev = last?.[last.length - 1];
    if (prev && prev.speaker === segment.speaker && segment.startedAt - prev.endedAt <= maxGapMs) {
      last.push(segment);
    } else {
      groups.push([segment]);
    }
  }
  return groups;
}

/** 会話形式の文字起こし */
export function TranscriptList({ segments, live, sources }: TranscriptListProps) {
  const listRef = useRef<HTMLDivElement>(null);
  const stickToBottom = useRef(true);

  // 末尾を見ているときだけ、新しい発話に合わせて自動でスクロールする
  useLayoutEffect(() => {
    const el = listRef.current;
    if (el && stickToBottom.current) el.scrollTop = el.scrollHeight;
  }, [segments.length]);

  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const onScroll = () => {
      stickToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
    };
    el.addEventListener("scroll", onScroll);
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  const groups = groupSegments(segments);

  return (
    <>
      <div className="transcript-list" ref={listRef}>
        {groups.length === 0 ? (
          <p className="transcript-empty">
            {live
              ? "話し始めると、数秒後にここへ表示されます。"
              : "録音を開始すると、会話の文字起こしがここに表示されます。"}
          </p>
        ) : (
          groups.map((group) => (
            <div key={group[0].id} className={`utterance from-${group[0].speaker}`}>
              <div className="utterance-meta">
                <span>{SPEAKER_LABEL[group[0].speaker]}</span>
                <time>{timeLabel(group[0].startedAt)}</time>
              </div>
              <div className="utterance-bubble">
                {group.map((s) => (
                  <p key={s.id}>{s.text}</p>
                ))}
              </div>
            </div>
          ))
        )}
      </div>
      {live && !sources.includes("them") && (
        <p className="panel-footnote">相手の声は取り込んでいません（マイクのみ）</p>
      )}
    </>
  );
}
