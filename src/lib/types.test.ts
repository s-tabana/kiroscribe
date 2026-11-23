import { describe, expect, it } from "vitest";
import defaultSettings from "../../crates/core/tests/fixtures/default-settings.json";
import { createMemoryBackend } from "./memoryBackend";
import { DEFAULT_SETTINGS } from "./types";

describe("設定の最初の値", () => {
  // 本体側（crates/core/tests/storage.rs）と同じ見本。片方だけを変えると、どちらかのテストが落ちる
  it("[SET-001] 画面側の最初の値は、本体と共通の見本と同じ", async () => {
    expect(DEFAULT_SETTINGS).toEqual(defaultSettings);
    expect(Object.keys(DEFAULT_SETTINGS).sort()).toEqual(Object.keys(defaultSettings).sort());
    // プレビュー・テスト用のバックエンドも同じ値から始まる
    expect(await createMemoryBackend().getSettings()).toEqual(defaultSettings);
  });
});
