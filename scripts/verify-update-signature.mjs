#!/usr/bin/env node
// ビルドでできた更新用の署名（.sig）が、アプリに埋め込む公開鍵で検証できるかを確かめる。
//
// 使い方:
//   node scripts/verify-update-signature.mjs \
//     --file target/release/bundle/nsis/Kiroscribe_0.2.0_x64-setup.exe \
//     --signature target/release/bundle/nsis/Kiroscribe_0.2.0_x64-setup.exe.sig \
//     [--config src-tauri/tauri.conf.json]
//
// なぜ要るか: 秘密鍵（Secret）と tauri.conf.json の公開鍵が対になっていなくても、
// Tauri のビルドは警告を出すだけで成功する。そのまま公開すると、利用者のアプリは
// 更新の知らせを出したあと、署名の検証で必ず失敗する。公開の前にここで止める。

import { existsSync, readFileSync } from "node:fs";
import { UsageError, isMain, readOptions, run } from "./lib/cli.mjs";
import { formatKeyId, verifySignature } from "./lib/minisign.mjs";

const USAGE =
  "node scripts/verify-update-signature.mjs --file <インストーラ> --signature <.sig のパス> [--config src-tauri/tauri.conf.json]";

function read(path, what, encoding) {
  try {
    return readFileSync(path, encoding);
  } catch (error) {
    throw new UsageError(`${what}（${path}）を読めません: ${error.message}`);
  }
}

/** tauri.conf.json から更新用の公開鍵を取り出す */
export function readPublicKey(configPath) {
  let config;
  try {
    config = JSON.parse(read(configPath, "Tauri の設定", "utf8"));
  } catch (error) {
    if (error instanceof UsageError) throw error;
    throw new UsageError(`${configPath} が JSON として読めません: ${error.message}`);
  }
  const pubkey = config?.plugins?.updater?.pubkey;
  if (typeof pubkey !== "string" || pubkey.trim() === "") {
    throw new UsageError(`${configPath} に plugins.updater.pubkey がありません`);
  }
  // Tauri CLI と同じく、値がファイルのパスならその中身を使う
  return existsSync(pubkey) ? read(pubkey, "公開鍵のファイル", "utf8") : pubkey;
}

function main(argv) {
  const options = readOptions(
    argv,
    { file: { required: true }, signature: { required: true }, config: {} },
    USAGE,
  );
  const pubkey = readPublicKey(options.config ?? "src-tauri/tauri.conf.json");
  const data = read(options.file, "署名するファイル");
  const sigText = read(options.signature, "署名ファイル", "utf8");
  const sig = verifySignature(data, sigText, pubkey);
  console.log(
    `署名を確認しました: ${options.file}（鍵 ID ${formatKeyId(sig.keyId)}、${sig.trustedComment.replaceAll("\t", " ")}）`,
  );
}

if (isMain(import.meta.url)) await run(main);
