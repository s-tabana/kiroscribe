#!/usr/bin/env node
// 配布するビルドに、E2E テスト用の差し替え口（cargo の feature `e2e`）が入っていないことを確かめる。
//
//   node e2e/check-release-binary.mjs target/release/kiroscribe.exe
//
// 差し替え口のコードだけが持つ文字列（作業フォルダを指す環境変数の名前など）を、実行ファイルの中から探す。
// 1 つでも見つかれば、feature `e2e` を付けてビルドしたものなので失敗にする。

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/** crates/core/src/e2e/ と src-tauri/src/e2e.rs にだけ現れる文字列 */
export const MARKERS = ["KIROSCRIBE_E2E_DIR", "dev.kiroscribe.e2e", "kiroscribe-e2e-", "dialogs.json", "close-window.txt"];

/** 実行ファイルの中身（Buffer）に含まれている、差し替え口の印 */
export function findMarkers(bytes) {
  // Rust の文字列は UTF-8、Windows の API に渡す文字列は UTF-16 で入る
  return MARKERS.filter(
    (marker) => bytes.includes(Buffer.from(marker, "utf8")) || bytes.includes(Buffer.from(marker, "utf16le")),
  );
}

function main(files) {
  if (files.length === 0) {
    console.error("使い方: node e2e/check-release-binary.mjs <実行ファイル>...");
    return 2;
  }
  let failed = false;
  for (const file of files) {
    const bytes = fs.readFileSync(file);
    const found = findMarkers(bytes);
    if (found.length > 0) {
      failed = true;
      console.error(`NG ${file}: E2E テスト用のコードが含まれています（${found.join(", ")}）`);
    } else {
      console.log(`OK ${file}: E2E テスト用のコードは含まれていません（${(bytes.length / 1024 / 1024).toFixed(1)} MB を検査）`);
    }
  }
  return failed ? 1 : 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main(process.argv.slice(2)));
}
