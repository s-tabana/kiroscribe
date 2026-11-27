import { useEffect, useLayoutEffect, useRef } from "react";
import { SPEAKER_LABEL, type Segment, type Speaker, type TranscriptBreak } from "../lib/types";
import { timeLabel } from "../lib/dates";

interface TranscriptListProps {
  segments: Segment[];
  /** 文字起こしの区切り（同じノートで録音を始め直した位置） */
  breaks: TranscriptBreak[];
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

/** 「2回目の録音（14:30〜）」の形の見出し */
export function roundLabel(round: number, startedAt: number): string {
  return `${round}回目の録音（${timeLabel(startedAt)}〜）`;
}

export interface TranscriptRound {
  /** 回の始まりの区切り。1 回目（区切りより前）は null */
  start: TranscriptBreak | null;
  segments: Segment[];
}

/** 開始時刻順の発話を、区切りで回ごとに分ける（区切りより前に始まった発話が 1 回目） */
export function splitRounds(segments: Segment[], breaks: TranscriptBreak[]): TranscriptRound[] {
  const ordered = [...breaks].sort((a, b) => a.startedAt - b.startedAt || a.id - b.id);
  const rounds: TranscriptRound[] = [{ start: null, segments: [] }];
  let next = 0;
  for (const segment of segments) {
    while (next < ordered.length && ordered[next].startedAt <= segment.startedAt) {
      rounds.push({ start: ordered[next], segments: [] });
      next += 1;
    }
    rounds[rounds.length - 1].segments.push(segment);
  }
  for (; next < ordered.length; next += 1) rounds.push({ start: ordered[next], segments: [] });
  return rounds;
}

/** 会話形式の文字起こし */
export function TranscriptList({ segments, breaks, live, sources }: TranscriptListProps) {
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

  // 発話のない回は表示しない。ただし録音中は、始まったばかりの回の見出しを出す
  const rounds = splitRounds(segments, breaks).filter(
    (round, index, all) => round.segments.length > 0 || (live && round.start && index === all.length - 1),
  );

  return (
    <>
      <div className="transcript-list" ref={listRef}>
        {segments.length === 0 ? (
          <p className="transcript-empty">
            {live
              ? "話し始めると、数秒後にここへ表示されます。"
              : "録音を開始すると、会話の文字起こしがここに表示されます。"}
          </p>
        ) : (
          rounds.map((round) => (
            <section key={round.start?.id ?? "first"} className="transcript-round">
              {round.start && (
                <h3 className="transcript-break">{roundLabel(round.start.round, round.start.startedAt)}</h3>
              )}
              {groupSegments(round.segments).map((group) => (
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
              ))}
            </section>
          ))
        )}
      </div>
      {live && !sources.includes("them") && (
        <p className="panel-footnote">相手の声は取り込んでいません（マイクのみ）</p>
      )}
    </>
  );
}
