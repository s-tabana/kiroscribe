#!/usr/bin/env node
// 本物のインストーラを使う、インストールと更新の通しのテストを走らせる: npm run test:e2e:installer
//
// 先に、テスト用のインストーラ（npm run build:e2e:installer）・tauri-driver・msedgedriver が要る
// （E2E テストと同じ。docs/development.md）。Windows でだけ動く。
//
// このテストは、この PC の利用者の場所（%LOCALAPPDATA%\Kiroscribe）に本当にインストールし、最後にアンインストールする。
// 製品の Kiroscribe が入っている PC で走らせると、それを E2E テスト用のビルドで置き換えてしまうので、
// インストール済みなら止める（CI のように使い捨ての環境では、--replace-installed を付けると続ける）。

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { ARTIFACTS_DIR, ROOT, tauriDriverBinary } from "../lib/app.mjs";
import { describeTools } from "../lib/diagnostics.mjs";
import { BUILD_INFO } from "./build.mjs";
import { installed } from "./windows.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));

function fail(message) {
  console.error(`\nインストーラのテストを始められません: ${message}\n`);
  process.exit(2);
}

if (process.platform !== "win32") fail("Windows で実行してください");
if (!fs.existsSync(BUILD_INFO)) fail(`テスト用のインストーラがありません（${BUILD_INFO}）。先に npm run build:e2e:installer を実行してください`);
if (!fs.existsSync(tauriDriverBinary())) fail(`tauri-driver がありません（${tauriDriverBinary()}）`);
if (!process.env.KIROSCRIBE_E2E_NATIVE_DRIVER && spawnSync("where", ["msedgedriver"], { stdio: "ignore" }).status !== 0) {
  fail("msedgedriver.exe が PATH にありません。WebView2 と同じバージョンのものを入れてください（docs/development.md）");
}
const existing = installed();
if (existing && !process.argv.includes("--replace-installed")) {
  fail(
    `この PC には Kiroscribe ${existing.displayVersion} がインストールされています（${existing.installLocation}）。` +
      "テストはこれを E2E テスト用のビルドで置き換え、最後にアンインストールします。" +
      "使い捨ての環境でだけ、--replace-installed を付けて実行してください",
  );
}

console.log(describeTools());
fs.rmSync(ARTIFACTS_DIR, { recursive: true, force: true });
const result = spawnSync(
  process.execPath,
  ["--test", "--test-concurrency=1", "--test-force-exit", "--test-reporter=spec", path.join(HERE, "update.test.mjs")],
  { cwd: ROOT, stdio: "inherit" },
);
if (result.status !== 0 && fs.existsSync(ARTIFACTS_DIR)) {
  console.error(`\n失敗したテストのスクリーンショットとログ: ${path.relative(ROOT, ARTIFACTS_DIR)}${path.sep}`);
}
process.exit(result.status ?? 1);
