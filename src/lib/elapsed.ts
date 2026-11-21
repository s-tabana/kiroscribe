import type { RecordingStatus } from "./types";

/** 一時停止を除いた録音時間（ミリ秒） */
export function elapsedMs(status: RecordingStatus, now: number = Date.now()): number {
  return status.elapsedMs + (status.runningSince !== null ? Math.max(0, now - status.runningSince) : 0);
}

/** 3:07 / 1:02:07 */
export function formatElapsed(ms: number): string {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}
