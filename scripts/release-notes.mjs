#!/usr/bin/env node
// CHANGELOG.md から、指定したバージョンの節の本文を取り出す。
// 取り出した本文は、GitHub のリリース本文と、アプリ内の「リリースノート」になる。
//
// 使い方:
//   node scripts/release-notes.mjs --version 0.2.0 [--changelog CHANGELOG.md] [--output <ファイル>]
//
// 節は「## 0.2.0 (2026-11-21)」の形の見出しで始まり、次の「## 」の手前で終わる。
// 見出し行そのものは出力しない。節がない・本文が空のときは、終了コード 1 で終わる。

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { UsageError, isMain, readOptions, run } from "./lib/cli.mjs";

const USAGE =
  "node scripts/release-notes.mjs --version <バージョン> [--changelog CHANGELOG.md] [--output <ファイル>]";

/** 「## 」の見出し行ならバージョンの部分を返す（「### 」などは対象外）。見出しでなければ null */
function headingVersion(line) {
  const match = /^##(?!#)\s*\[?v?([^\s\]()]*)/.exec(line);
  return match ? match[1] : null;
}

/** CHANGELOG に書かれているバージョンを、上から順に返す */
export function listVersions(changelog) {
  return sections(changelog).map((section) => section.version);
}

/** version の節の本文（見出しを除く）を返す。なければ UsageError */
export function extractReleaseNotes(changelog, version) {
  const wanted = version.trim().replace(/^v/, "");
  if (wanted === "") throw new UsageError("バージョンが空です");

  const all = sections(changelog);
  const section = all.find((candidate) => candidate.version === wanted);
  if (!section) {
    const found = all.map((candidate) => candidate.version).join("、") || "なし";
    throw new UsageError(
      `CHANGELOG に「## ${wanted} (日付)」の節がありません（見つかった節: ${found}）`,
    );
  }

  const body = [...section.lines];
  while (body.length > 0 && body[0].trim() === "") body.shift();
  while (body.length > 0 && body.at(-1).trim() === "") body.pop();
  if (body.length === 0) {
    throw new UsageError(`CHANGELOG の「## ${wanted}」の節に本文がありません`);
  }
  return `${body.join("\n")}\n`;
}

/** CHANGELOG を「## 」の見出しごとに分ける */
function sections(changelog) {
  // Windows でチェックアウトすると改行が CRLF になるので、どちらでも読めるようにする
  const lines = changelog.replace(/^﻿/, "").split(/\r?\n/);
  const result = [];
  let current = null;
  let fence = null; // コードブロックの中では「## 」を見出しとみなさない
  for (const line of lines) {
    const fenceMatch = /^\s{0,3}(`{3,}|~{3,})/.exec(line);
    if (fenceMatch) {
      if (fence === null) fence = fenceMatch[1][0];
      else if (fence === fenceMatch[1][0]) fence = null;
    } else if (fence === null) {
      const version = headingVersion(line);
      if (version !== null) {
        current = { version, lines: [] };
        result.push(current);
        continue;
      }
    }
    current?.lines.push(line);
  }
  return result;
}

function main(argv) {
  const options = readOptions(
    argv,
    { version: { required: true }, changelog: {}, output: {} },
    USAGE,
  );
  const changelogPath = options.changelog ?? "CHANGELOG.md";
  let changelog;
  try {
    changelog = readFileSync(changelogPath, "utf8");
  } catch (error) {
    throw new UsageError(`${changelogPath} を読めません: ${error.message}`);
  }
  const notes = extractReleaseNotes(changelog, options.version);
  if (options.output) {
    mkdirSync(dirname(options.output), { recursive: true });
    writeFileSync(options.output, notes);
  } else {
    process.stdout.write(notes);
  }
}

if (isMain(import.meta.url)) await run(main);
