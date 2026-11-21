import { describe, expect, it } from "vitest";
import {
  dayLabel,
  fullDateLabel,
  groupByDay,
  releaseDateLabel,
  shortDateTimeLabel,
  timeLabel,
} from "./dates";

// 2026-11-20（金）10:00 ローカル時刻
const NOW = new Date(2026, 10, 20, 10, 0).getTime();
const at = (month: number, day: number, hour = 9, minute = 0, year = 2026) =>
  new Date(year, month - 1, day, hour, minute).getTime();

describe("dayLabel", () => {
  it("今日・昨日・それ以前を区別する", () => {
    expect(dayLabel(at(11, 20, 0, 5), NOW)).toBe("今日");
    expect(dayLabel(at(11, 19, 23, 59), NOW)).toBe("昨日");
    expect(dayLabel(at(11, 17), NOW)).toBe("11月17日（火）");
  });

  it("前年は年を付ける", () => {
    expect(dayLabel(at(12, 31, 9, 0, 2025), NOW)).toBe("2025年12月31日（水）");
  });
});

describe("timeLabel / fullDateLabel", () => {
  it("分はゼロ埋めする", () => {
    expect(timeLabel(at(11, 20, 9, 5))).toBe("9:05");
    expect(fullDateLabel(at(11, 20, 15, 4))).toBe("2026年11月20日（金） 15:04");
  });
});

describe("groupByDay", () => {
  it("連続する同じ日のノートをまとめる", () => {
    const items = [
      { id: "a", createdAt: at(11, 20, 9) },
      { id: "b", createdAt: at(11, 20, 8) },
      { id: "c", createdAt: at(11, 19, 18) },
      { id: "d", createdAt: at(11, 3) },
    ];
    const groups = groupByDay(items, NOW);
    expect(groups.map((g) => g.label)).toEqual(["今日", "昨日", "11月3日（火）"]);
    expect(groups[0].items.map((i) => i.id)).toEqual(["a", "b"]);
  });
});

describe("shortDateTimeLabel", () => {
  it("月/日 時:分", () => {
    expect(shortDateTimeLabel(at(11, 20, 15, 4))).toBe("11/20 15:04");
  });
});

describe("releaseDateLabel", () => {
  it("日付だけのとき（同梱の変更履歴）はそのまま読む", () => {
    expect(releaseDateLabel("2026-11-21")).toBe("2026年11月21日");
    expect(releaseDateLabel(" 2026-1-5 ")).toBe("2026年1月5日");
  });

  it("日時（GitHub のリリース）はこの PC の日付にする", () => {
    const published = new Date(2026, 11, 1, 9, 30);
    expect(releaseDateLabel(published.toISOString())).toBe("2026年12月1日");
  });

  it("日付がない・読めないとき", () => {
    expect(releaseDateLabel(null)).toBeNull();
    expect(releaseDateLabel("  ")).toBeNull();
    expect(releaseDateLabel("近日公開")).toBe("近日公開");
  });
});
