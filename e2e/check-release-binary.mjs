#!/usr/bin/env node
// 配布するビルドに、E2E テスト用の差し替え口（cargo の feature `e2e`）が入っていないことを確かめる。
//
//   node e2e/check-release-binary.mjs target/release/kiroscribe.exe
//
// 差し替え口のコードだけが持つ文字列（作業フォルダを指す環境変数の名前など）を、実行ファイルの中から探す。
// 1 つでも見つかれば、feature `e2e` を付けてビルドしたものなので失敗にする。

import fs from "node:fs";

/** crates/core/src/e2e/ と src-tauri/src/e2e.rs にだけ現れる文字列 */
const MARKERS = ["KIROSCRIBE_E2E_DIR", "dev.kiroscribe.e2e", "kiroscribe-e2e-", "dialogs.json", "close-window.txt"];

const files = process.argv.slice(2);
if (files.length === 0) {
  console.error("使い方: node e2e/check-release-binary.mjs <実行ファイル>...");
  process.exit(2);
}

let failed = false;
for (const file of files) {
  const bytes = fs.readFileSync(file);
  // Rust の文字列は UTF-8、Windows の API に渡す文字列は UTF-16 で入る
  const found = MARKERS.filter(
    (marker) => bytes.includes(Buffer.from(marker, "utf8")) || bytes.includes(Buffer.from(marker, "utf16le")),
  );
  if (found.length > 0) {
    failed = true;
    console.error(`NG ${file}: E2E テスト用のコードが含まれています（${found.join(", ")}）`);
  } else {
    console.log(`OK ${file}: E2E テスト用のコードは含まれていません（${(bytes.length / 1024 / 1024).toFixed(1)} MB を検査）`);
  }
}
process.exit(failed ? 1 : 0);
