import { useEffect, useState } from "react";

/** active のあいだ interval ミリ秒ごとに現在時刻を更新する */
export function useNow(active: boolean, interval = 500): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), interval);
    return () => clearInterval(timer);
  }, [active, interval]);
  return now;
}
