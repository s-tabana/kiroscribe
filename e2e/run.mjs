#!/usr/bin/env node
// E2E テストを走らせる: npm run test:e2e [-- <テストのファイル>...]
//
// 1. 模擬サーバーのテスト（e2e/lib/*.test.mjs。アプリは起動しない）
// 2. アプリを起動して操作するテスト（e2e/specs/*.test.mjs）。1 件ずつ順に走らせる
//
// 先に、E2E テスト用のアプリ（npm run build:e2e）と tauri-driver（cargo install tauri-driver --locked）が要る。
// Linux では、画面のない環境でも動くよう、仮想の画面（xvfb-run）と D-Bus のセッション（dbus-run-session。
// 多重起動の防止が使う）の中で自分自身を起動し直す。

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { appBinary, ARTIFACTS_DIR, ROOT, tauriDriverBinary } from "./lib/app.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const IS_LINUX = process.platform === "linux";
const IS_WINDOWS = process.platform === "win32";

function fail(message) {
  console.error(`\nE2E テストを始められません: ${message}\n`);
  process.exit(2);
}

function onPath(command) {
  const which = spawnSync(IS_WINDOWS ? "where" : "which", [command], { stdio: "ignore" });
  return which.status === 0;
}

function testFiles(dir) {
  return fs
    .readdirSync(dir)
    .filter((name) => name.endsWith(".test.mjs"))
    .sort()
    .map((name) => path.join(dir, name));
}

// ---- 前提の確認 ----

if (!fs.existsSync(appBinary())) {
  fail(`E2E テスト用のアプリがありません（${appBinary()}）。先に npm run build:e2e を実行してください`);
}
if (!fs.existsSync(tauriDriverBinary())) {
  fail(`tauri-driver がありません（${tauriDriverBinary()}）。cargo install tauri-driver --locked で入れてください`);
}
if (!process.env.KIROSCRIBE_E2E_NATIVE_DRIVER) {
  const native = IS_WINDOWS ? "msedgedriver" : "WebKitWebDriver";
  if (!onPath(native)) {
    fail(
      IS_WINDOWS
        ? "msedgedriver.exe が PATH にありません。WebView2 と同じバージョンのものを入れてください（docs/development.md）"
        : "WebKitWebDriver が PATH にありません。Debian/Ubuntu では webkit2gtk-driver パッケージを入れてください",
    );
  }
}

// ---- Linux: 仮想の画面と D-Bus のセッションの中で起動し直す ----

if (IS_LINUX && !process.env.KIROSCRIBE_E2E_WRAPPED) {
  const wrap = [];
  if (!process.env.DBUS_SESSION_BUS_ADDRESS) {
    if (!onPath("dbus-run-session")) fail("dbus-run-session がありません（dbus パッケージ）");
    wrap.push("dbus-run-session", "--");
  }
  if (!process.env.DISPLAY && !process.env.WAYLAND_DISPLAY) {
    if (!onPath("xvfb-run")) fail("画面がありません。xvfb パッケージを入れるか、画面のある環境で実行してください");
    wrap.push("xvfb-run", "--auto-servernum", "--server-args=-screen 0 1440x900x24");
  }
  if (wrap.length > 0) {
    const [command, ...args] = [...wrap, process.execPath, fileURLToPath(import.meta.url), ...process.argv.slice(2)];
    const result = spawnSync(command, args, { stdio: "inherit", env: { ...process.env, KIROSCRIBE_E2E_WRAPPED: "1" } });
    process.exit(result.status ?? 1);
  }
}

// ---- 実行 ----

const requested = process.argv.slice(2).map((file) => path.resolve(file));
const files = requested.length > 0 ? requested : [...testFiles(path.join(HERE, "lib")), ...testFiles(path.join(HERE, "specs"))];

fs.rmSync(ARTIFACTS_DIR, { recursive: true, force: true });
const result = spawnSync(
  process.execPath,
  // 1 件ずつ順に（同時に走らせると、遅い環境で待ち時間が足りなくなる）。終わったら、残った接続があっても終了する
  ["--test", "--test-concurrency=1", "--test-force-exit", "--test-reporter=spec", ...files],
  { cwd: ROOT, stdio: "inherit" },
);
if (result.status !== 0 && fs.existsSync(ARTIFACTS_DIR)) {
  console.error(`\n失敗したテストのスクリーンショットとログ: ${path.relative(ROOT, ARTIFACTS_DIR)}${path.sep}`);
}
process.exit(result.status ?? 1);
