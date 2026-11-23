#!/usr/bin/env node
// 本物のインストーラを使う更新のテスト（e2e/installer/）の材料を作る: npm run build:e2e:installer
//
// 同じソースから、バージョンだけを変えたインストーラ（NSIS）を 2 つ作る。
//   - 古い版（OLD_VERSION）: 先にインストールして、ここから更新する
//   - 新しい版（NEW_VERSION）: 模擬サーバーから配る更新
// どちらも E2E テスト用のビルド（cargo の feature `e2e`）で、使い捨ての署名鍵で署名し、その公開鍵をアプリに埋め込む
// （リリースと同じく、tauri.conf.json の plugins.updater.pubkey に入れる）。本番の鍵には触れない。
//
// できるもの（target/installer-e2e/）:
//   Kiroscribe_<版>_x64-setup.exe・.sig … インストーラと、更新用の署名（リリースと同じ形式）
//   update.key・update.key.pub        … 使い捨ての署名鍵
//   build.json                        … 上の一覧（テストが読む）
//
// Windows でだけ動く（NSIS のインストーラは Windows でしか作れない）。

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const TAURI_CLI = path.join(ROOT, "node_modules", "@tauri-apps", "cli", "tauri.js");
export const OUT_DIR = path.join(ROOT, "target", "installer-e2e");
export const BUILD_INFO = path.join(OUT_DIR, "build.json");

/** テストに使うバージョン。製品のバージョンとは関係なく、新しい版のほうが大きければよい */
export const OLD_VERSION = "0.0.1";
export const NEW_VERSION = "0.0.2";

function fail(message) {
  console.error(`\nテスト用のインストーラを作れません: ${message}\n`);
  process.exit(1);
}

/** 本番の署名鍵が環境変数にあっても、テストのビルドには渡さない */
function envWithoutSigningKeys() {
  const env = { ...process.env };
  for (const name of Object.keys(env)) {
    if (name.startsWith("TAURI_SIGNING_")) delete env[name];
  }
  return env;
}

function tauri(args, env = envWithoutSigningKeys()) {
  console.log(`\n$ tauri ${args.join(" ")}`);
  const result = spawnSync(process.execPath, [TAURI_CLI, ...args], { cwd: ROOT, env, stdio: "inherit" });
  if (result.status !== 0) fail(`tauri ${args.slice(0, 2).join(" ")} が失敗しました（終了コード ${result.status}）`);
}

function main() {
  if (process.platform !== "win32") fail("Windows で実行してください（NSIS のインストーラは Windows でしか作れません）");
  if (!fs.existsSync(TAURI_CLI)) fail("Tauri CLI がありません。先に npm ci を実行してください");

  fs.rmSync(OUT_DIR, { recursive: true, force: true });
  fs.mkdirSync(OUT_DIR, { recursive: true });

  // 使い捨ての署名鍵（パスワードなし）
  const keyPath = path.join(OUT_DIR, "update.key");
  tauri(["signer", "generate", "--ci", "--force", "-w", keyPath]);
  const pubkey = fs.readFileSync(`${keyPath}.pub`, "utf8").trim();
  const signingEnv = {
    ...envWithoutSigningKeys(),
    TAURI_SIGNING_PRIVATE_KEY: fs.readFileSync(keyPath, "utf8").trim(),
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "",
  };

  const versions = {};
  for (const [role, version] of [
    ["old", OLD_VERSION],
    ["new", NEW_VERSION],
  ]) {
    // リリースと同じ設定（tauri.conf.json）に、バージョンと公開鍵だけを重ねる。
    // 更新用の署名（createUpdaterArtifacts）は tauri.conf.json のとおり作る
    const configPath = path.join(OUT_DIR, `tauri.${role}.conf.json`);
    fs.writeFileSync(
      configPath,
      JSON.stringify({ version, bundle: { createUpdaterArtifacts: true }, plugins: { updater: { pubkey } } }, null, 2),
    );
    tauri(["build", "--debug", "--features", "e2e", "--bundles", "nsis", "--ci", "--config", configPath], signingEnv);

    const name = `Kiroscribe_${version}_x64-setup.exe`;
    const built = path.join(ROOT, "target", "debug", "bundle", "nsis", name);
    if (!fs.existsSync(built)) fail(`${built} ができていません`);
    if (!fs.existsSync(`${built}.sig`)) fail(`${built}.sig（更新用の署名）ができていません`);
    const installer = path.join(OUT_DIR, name);
    fs.copyFileSync(built, installer);
    fs.copyFileSync(`${built}.sig`, `${installer}.sig`);
    versions[role] = { version, installer, signature: `${installer}.sig` };
  }

  fs.writeFileSync(BUILD_INFO, JSON.stringify({ pubkey, key: keyPath, ...versions }, null, 2));
  console.log(`\nテスト用のインストーラを作りました: ${OUT_DIR}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
