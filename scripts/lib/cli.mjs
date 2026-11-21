// リリース用スクリプトに共通の道具（引数の読み取り・エラーの表示・直接実行の判定）

import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

/** 入力の誤りなど、スタックトレースなしでメッセージだけ見せたいエラー */
export class UsageError extends Error {}

/** そのファイルが `node scripts/xxx.mjs` として直接実行されたか（import されただけなら false） */
export function isMain(metaUrl) {
  if (!process.argv[1]) return false;
  try {
    // Windows のドライブ文字の大小やシンボリックリンクの違いを吸収して比べる
    return (
      realpathSync.native(fileURLToPath(metaUrl)) ===
      realpathSync.native(process.argv[1])
    );
  } catch {
    return false;
  }
}

/**
 * `--名前 値` の形の引数を読む。
 * spec は { 名前: { required?: boolean } }。知らない引数・足りない引数は UsageError にする。
 */
export function readOptions(argv, spec, usage) {
  const options = Object.fromEntries(
    Object.keys(spec).map((name) => [name, { type: "string" }]),
  );
  let values;
  try {
    ({ values } = parseArgs({ args: argv, options, strict: true, allowPositionals: false }));
  } catch (error) {
    throw new UsageError(`${error.message}\n使い方: ${usage}`);
  }
  const missing = Object.keys(spec).filter(
    (name) => spec[name].required && !values[name],
  );
  if (missing.length > 0) {
    throw new UsageError(
      `引数が足りません: ${missing.map((name) => `--${name}`).join(" ")}\n使い方: ${usage}`,
    );
  }
  return values;
}

/** main を実行し、UsageError ならメッセージだけ出して終了コード 1 にする */
export async function run(main) {
  try {
    await main(process.argv.slice(2));
  } catch (error) {
    if (!(error instanceof UsageError)) throw error;
    if (process.env.GITHUB_ACTIONS === "true") {
      // GitHub Actions では実行結果の画面に注釈として表示させる（改行は %0A で表す）
      const escaped = error.message
        .replaceAll("%", "%25")
        .replaceAll("\r", "%0D")
        .replaceAll("\n", "%0A");
      console.error(`::error::${escaped}`);
    } else {
      console.error(`エラー: ${error.message}`);
    }
    process.exitCode = 1;
  }
}
