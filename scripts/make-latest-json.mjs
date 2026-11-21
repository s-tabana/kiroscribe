#!/usr/bin/env node
// アプリ内更新（Tauri の updater）が読む latest.json を作る。
//
// 使い方:
//   node scripts/make-latest-json.mjs \
//     --version 0.2.0 \
//     --installer Kiroscribe_0.2.0_x64-setup.exe \
//     --signature target/release/bundle/nsis/Kiroscribe_0.2.0_x64-setup.exe.sig \
//     --notes release-notes.md \
//     --repo s-tabana/kiroscribe \
//     [--pub-date 2026-11-21T09:00:00Z] [--output latest.json]
//
// 形式は tauri-plugin-updater の「静的な JSON」:
//   { version, notes, pub_date, platforms: { "windows-x86_64": { signature, url } } }
// - signature は .sig ファイルの「中身」（パスや URL ではない）
// - url は GitHub Releases に添付したインストーラの URL
// - pub_date は RFC 3339

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { UsageError, isMain, readOptions, run } from "./lib/cli.mjs";
import { parseSignature } from "./lib/minisign.mjs";

const USAGE =
  "node scripts/make-latest-json.mjs --version <バージョン> --installer <ファイル名> --signature <.sig のパス> --notes <本文のファイル> --repo <owner/name> [--pub-date <RFC 3339>] [--output <ファイル>]";

// updater が Windows（64 ビット）で探すキー。「windows-x86_64-nsis」がなければこれを使う
const WINDOWS_TARGET = "windows-x86_64";

const SEMVER =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/;
const RFC3339 = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$/;

/** いまの時刻を RFC 3339（秒まで・UTC）で返す */
function nowRfc3339() {
  return new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
}

/**
 * latest.json の中身（オブジェクト）を作る。
 * signature は .sig ファイルの中身、installer は添付するインストーラのファイル名。
 */
export function buildLatestJson({ version, installer, signature, notes, repo, pubDate }) {
  const cleanVersion = String(version ?? "").trim().replace(/^v/, "");
  if (!SEMVER.test(cleanVersion)) {
    throw new UsageError(`バージョン「${version}」が 1.2.3 の形（SemVer）ではありません`);
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9._-]+$/.test(repo ?? "")) {
    throw new UsageError(`リポジトリ「${repo}」が owner/name の形ではありません`);
  }
  // GitHub は添付ファイル名の空白などを別の文字に置き換えるので、URL とずれない文字だけを許す
  if (!/^[A-Za-z0-9._-]+$/.test(installer ?? "")) {
    throw new UsageError(
      `インストーラのファイル名「${installer}」に、英数字と . _ - 以外の文字があります（パスではなくファイル名だけを渡してください）`,
    );
  }
  const date = pubDate ?? nowRfc3339();
  if (!RFC3339.test(date) || Number.isNaN(Date.parse(date))) {
    throw new UsageError(`日時「${date}」が RFC 3339（例: 2026-11-21T09:00:00Z）ではありません`);
  }

  const cleanSignature = String(signature ?? "").trim();
  const parsed = parseSignature(cleanSignature);
  // 別のファイル・別のバージョンの署名を載せると、利用者のアプリが更新に失敗する。
  // Tauri CLI は署名の中にファイル名とバージョンを書き込むので、取り違えをここで止める
  if (parsed.fields.file !== undefined && parsed.fields.file !== installer) {
    throw new UsageError(
      `署名は「${parsed.fields.file}」のものですが、インストーラは「${installer}」です`,
    );
  }
  if (
    parsed.fields.version !== undefined &&
    parsed.fields.version.replace(/^v/, "") !== cleanVersion
  ) {
    throw new UsageError(
      `署名はバージョン ${parsed.fields.version} のものですが、公開するバージョンは ${cleanVersion} です`,
    );
  }

  return {
    version: cleanVersion,
    notes: String(notes ?? "").replace(/\r\n/g, "\n").trim(),
    pub_date: date,
    platforms: {
      [WINDOWS_TARGET]: {
        signature: cleanSignature,
        url: `https://github.com/${repo}/releases/download/v${encodeURIComponent(cleanVersion)}/${encodeURIComponent(installer)}`,
      },
    },
  };
}

function readText(path, what) {
  try {
    return readFileSync(path, "utf8");
  } catch (error) {
    throw new UsageError(`${what}（${path}）を読めません: ${error.message}`);
  }
}

function main(argv) {
  const options = readOptions(
    argv,
    {
      version: { required: true },
      installer: { required: true },
      signature: { required: true },
      notes: { required: true },
      repo: { required: true },
      "pub-date": {},
      output: {},
    },
    USAGE,
  );
  const latest = buildLatestJson({
    version: options.version,
    installer: options.installer,
    signature: readText(options.signature, "署名ファイル"),
    notes: readText(options.notes, "リリースノート"),
    repo: options.repo,
    pubDate: options["pub-date"],
  });
  const json = `${JSON.stringify(latest, null, 2)}\n`;
  if (options.output) {
    mkdirSync(dirname(options.output), { recursive: true });
    writeFileSync(options.output, json);
  } else {
    process.stdout.write(json);
  }
}

if (isMain(import.meta.url)) await run(main);
