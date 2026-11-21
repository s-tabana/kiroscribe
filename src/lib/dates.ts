const WEEKDAYS = ["日", "月", "火", "水", "木", "金", "土"];

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** サイドバーの見出し: 今日 / 昨日 / 11月18日（火） / 2025年3月2日（日） */
export function dayLabel(ms: number, now: number = Date.now()): string {
  const day = startOfDay(ms);
  const today = startOfDay(now);
  const diffDays = Math.round((today - day) / 86_400_000);
  if (diffDays === 0) return "今日";
  if (diffDays === 1) return "昨日";
  const d = new Date(ms);
  const md = `${d.getMonth() + 1}月${d.getDate()}日（${WEEKDAYS[d.getDay()]}）`;
  return d.getFullYear() === new Date(now).getFullYear() ? md : `${d.getFullYear()}年${md}`;
}

/** 15:04 */
export function timeLabel(ms: number): string {
  const d = new Date(ms);
  return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/** ノート上部のメタ情報: 2026年11月20日（木） 15:04 */
export function fullDateLabel(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日（${WEEKDAYS[d.getDay()]}） ${timeLabel(ms)}`;
}

/** 注記用の短い日時: 11/20 15:04 */
export function shortDateTimeLabel(ms: number): string {
  const d = new Date(ms);
  return `${d.getMonth() + 1}/${d.getDate()} ${timeLabel(ms)}`;
}

export interface DayGroup<T> {
  label: string;
  items: T[];
}

/** createdAt の新しい順に並んだ配列を、日ごとのグループにまとめる */
export function groupByDay<T extends { createdAt: number }>(
  items: T[],
  now: number = Date.now(),
): DayGroup<T>[] {
  const groups: DayGroup<T>[] = [];
  for (const item of items) {
    const label = dayLabel(item.createdAt, now);
    const last = groups[groups.length - 1];
    if (last && last.label === label) last.items.push(item);
    else groups.push({ label, items: [item] });
  }
  return groups;
}

/** リリースの公開日: 2026年11月21日。ISO 8601 の日時と、日付だけ（2026-11-21）の両方を受け付ける */
export function releaseDateLabel(publishedAt: string | null): string | null {
  const text = publishedAt?.trim();
  if (!text) return null;
  // 日付だけの場合は、時差で前後の日にずれないようそのまま読む
  const dateOnly = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(text);
  if (dateOnly) return `${Number(dateOnly[1])}年${Number(dateOnly[2])}月${Number(dateOnly[3])}日`;
  const ms = Date.parse(text);
  if (Number.isNaN(ms)) return text;
  const d = new Date(ms);
  return `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日`;
}
