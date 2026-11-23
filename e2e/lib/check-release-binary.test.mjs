// 配布するビルドの検査（e2e/check-release-binary.mjs）のテスト（アプリは起動しない）。
// 検査が「E2E テスト用のビルド」を見分けられること、配布するビルドが手元にあれば、それに差し替え口がないことを確かめる。

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { MARKERS, findMarkers } from "../check-release-binary.mjs";
import { ROOT, appBinary } from "./app.mjs";

const SCRIPT = path.join(ROOT, "e2e", "check-release-binary.mjs");
const EXE = process.platform === "win32" ? ".exe" : "";
/** 配布する形でビルドした実行ファイル（npm run tauri build や cargo build --release でできる） */
const RELEASE_BINARY = path.join(ROOT, "target", "release", `kiroscribe${EXE}`);

test("[SEC-012] 配布するビルドの検査は、テスト用の差し替え口が入ったビルドを見分ける", () => {
  // E2E テスト用のビルド（このテストが起動するアプリ）には、差し替え口の印がすべて入っている。
  // 印にしている文字列がコードから消えると、検査が素通しになるので、ここで気づけるようにする
  const e2eBuild = fs.readFileSync(appBinary());
  assert.deepEqual(
    findMarkers(e2eBuild),
    MARKERS,
    `${appBinary()} が E2E テスト用のビルドではありません（npm run build:e2e をやり直してください）`,
  );
  const refused = spawnSync(process.execPath, [SCRIPT, appBinary()], { encoding: "utf8" });
  assert.equal(refused.status, 1, refused.stdout + refused.stderr);
  assert.match(refused.stderr, /E2E テスト用のコードが含まれています/);

  // 印は、UTF-8 でも UTF-16（Windows の API に渡す文字列）でも見つける。印のない中身は通す
  assert.deepEqual(findMarkers(Buffer.from("....KIROSCRIBE_E2E_DIR....", "utf8")), ["KIROSCRIBE_E2E_DIR"]);
  assert.deepEqual(findMarkers(Buffer.from("....dialogs.json....", "utf16le")), ["dialogs.json"]);
  assert.deepEqual(findMarkers(Buffer.from("Kiroscribe の配布するビルド", "utf8")), []);
});

test(
  "[SEC-012] 配布するビルドには、テスト用の差し替え口が入っていない",
  { skip: fs.existsSync(RELEASE_BINARY) ? false : `配布するビルドがありません（${path.relative(ROOT, RELEASE_BINARY)}）。CI では ci.yml と release.yml が、ビルドした実行ファイルを検査します` },
  () => {
    assert.deepEqual(findMarkers(fs.readFileSync(RELEASE_BINARY)), []);
    const accepted = spawnSync(process.execPath, [SCRIPT, RELEASE_BINARY], { encoding: "utf8" });
    assert.equal(accepted.status, 0, accepted.stdout + accepted.stderr);
  },
);
