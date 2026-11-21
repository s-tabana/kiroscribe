import { useEffect, useState } from "react";
import type { Backend } from "../lib/backend";
import { IDLE_STATUS, type RecordingStatus } from "../lib/types";

/** 録音状態をバックエンドのイベントと同期して保持する */
export function useRecordingStatus(backend: Backend) {
  const [status, setStatus] = useState<RecordingStatus>(IDLE_STATUS);
  useEffect(() => {
    let active = true;
    const unsubscribe = backend.onRecordingStatus((next) => active && setStatus(next));
    void backend.getRecordingStatus().then((next) => active && setStatus(next));
    return () => {
      active = false;
      unsubscribe();
    };
  }, [backend]);
  return [status, setStatus] as const;
}
