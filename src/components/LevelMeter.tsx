import { useEffect, useState } from "react";
import { useBackend } from "../lib/backend";
import { SPEAKER_LABEL, type Speaker } from "../lib/types";

const BARS = 5;

/** 入力レベル（RMS）を 0〜BARS 本に変換する。小さな声も見えるよう対数的に扱う */
export function levelToBars(level: number): number {
  if (level <= 0.003) return 0;
  const db = 20 * Math.log10(level); // -50dB〜-10dB を 1〜5 本に
  return Math.max(1, Math.min(BARS, Math.round(((db + 50) / 40) * BARS)));
}

/** 話者ごとの入力レベル。レベルは頻繁に届くので、このコンポーネントだけが再描画される */
export function LevelMeter({ speaker }: { speaker: Speaker }) {
  const backend = useBackend();
  const [bars, setBars] = useState(0);

  useEffect(
    () => backend.onLevel((e) => e.speaker === speaker && setBars(levelToBars(e.level))),
    [backend, speaker],
  );

  return (
    <span className={`level level-${speaker}`} title={`${SPEAKER_LABEL[speaker]}の入力レベル`}>
      <span className="level-label">{SPEAKER_LABEL[speaker]}</span>
      <span className="level-bars" aria-hidden="true">
        {Array.from({ length: BARS }, (_, i) => (
          <i key={i} className={i < bars ? "on" : ""} style={{ height: `${6 + i * 3}px` }} />
        ))}
      </span>
    </span>
  );
}
