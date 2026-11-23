// アプリ内更新のテスト用の材料: 使い捨ての署名鍵と、それで署名した「更新ファイル」。
//
// 鍵と署名は、リリースと同じ Tauri CLI（tauri signer）で作る。鍵はテストのたびに作業フォルダの中に作って捨てる。
// 本番の鍵（GitHub の Secret）には触れない。

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { ROOT } from "./app.mjs";

const TAURI_CLI = path.join(ROOT, "node_modules", "@tauri-apps", "cli", "tauri.js");

function tauri(args) {
  // 本番の鍵が環境変数にあっても使わない
  const env = { ...process.env };
  for (const name of Object.keys(env)) {
    if (name.startsWith("TAURI_SIGNING_")) delete env[name];
  }
  const result = spawnSync(process.execPath, [TAURI_CLI, ...args], { cwd: ROOT, env, encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`tauri ${args.slice(0, 2).join(" ")} が失敗しました:\n${result.stdout}\n${result.stderr}`);
  }
}

/** 使い捨ての署名鍵を作る。pubkey は、アプリの設定（updatePubkey）に渡す公開鍵 */
export function createSigningKey(dir, name = "update") {
  const keyPath = path.join(dir, `${name}.key`);
  tauri(["signer", "generate", "--ci", "--force", "-w", keyPath]);
  return { keyPath, pubkey: fs.readFileSync(`${keyPath}.pub`, "utf8").trim() };
}

/** ファイルに署名して、latest.json の signature に書く文字列を返す */
export function signFile(key, file, version) {
  tauri(["signer", "sign", "-f", key.keyPath, "-p", "", "--app-version", version, file]);
  return fs.readFileSync(`${file}.sig`, "utf8").trim();
}

/**
 * 更新ファイルの中身。本物のインストーラではなく、ただの文字。
 * アプリは、これをダウンロードして署名を確かめたあと、インストールできる形式ではないと判断して止まる
 * （Windows でも Linux でも、何も実行されない）。
 */
export function updatePayload() {
  return Buffer.from("Kiroscribe の E2E テスト用の更新ファイル（インストーラではありません）\n".repeat(2000));
}

/**
 * 模擬サーバーに更新を公開する。signedBy で署名し、どの環境向けにも同じファイルを配る。
 * 返り値: { fileName, bytes }
 */
export function publishUpdate(server, dir, { version, signedBy, notes = "- 模擬サーバーの更新" }) {
  const fileName = `Kiroscribe_${version}_e2e.bin`;
  const file = path.join(dir, fileName);
  const bytes = updatePayload();
  fs.writeFileSync(file, bytes);
  publishFile(server, { version, fileName, bytes, signature: signFile(signedBy, file, version), notes });
  return { fileName, bytes };
}

/**
 * 署名済みのファイルを、そのまま模擬サーバーに更新として公開する（latest.json と、ファイルの配布）。
 * signature は latest.json の signature に書く文字列（.sig ファイルの中身）。どの環境向けにも同じファイルを配る
 */
export function publishFile(server, { version, fileName, bytes, signature, notes = "- 模擬サーバーの更新" }) {
  const platform = { signature, url: `${server.url}/update/files/${encodeURIComponent(fileName)}` };
  const platforms = {};
  for (const os of ["linux", "windows", "darwin"]) {
    for (const arch of ["x86_64", "aarch64", "i686", "armv7"]) platforms[`${os}-${arch}`] = platform;
  }
  server.update.files.set(fileName, bytes);
  server.update.manifest = { version, notes, pub_date: "2026-12-01T00:00:00Z", platforms };
}
