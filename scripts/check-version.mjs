#!/usr/bin/env node
// バージョンの書き間違いを、時間のかかるビルドの前に見つける。
//
// 使い方:
//   node scripts/check-version.mjs [--tag v0.2.0] [--root <リポジトリのルート>]
//
// 確かめること:
// - package.json（正）と、Cargo.toml の [workspace.package]、src-tauri/tauri.conf.json の
//   バージョンが同じであること（tauri.conf.json が "../package.json" を指していれば、その先を読む）
// - --tag を渡したときは、タグが「v + package.json のバージョン」であること
//
// 問題がなければバージョン（例: 0.2.0）だけを標準出力に書く。

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { UsageError, isMain, readOptions, run } from "./lib/cli.mjs";

const USAGE = "node scripts/check-version.mjs [--tag <タグ>] [--root <リポジトリのルート>]";

function readText(path) {
  try {
    return readFileSync(path, "utf8");
  } catch (error) {
    throw new UsageError(`${path} を読めません: ${error.message}`);
  }
}

function readJson(path) {
  try {
    return JSON.parse(readText(path));
  } catch (error) {
    if (error instanceof UsageError) throw error;
    throw new UsageError(`${path} が JSON として読めません: ${error.message}`);
  }
}

/** Cargo.toml の [workspace.package]（なければ [package]）の version を返す。なければ null */
export function cargoVersion(toml) {
  for (const wanted of ["workspace.package", "package"]) {
    let inSection = false;
    for (const line of toml.split(/\r?\n/)) {
      const header = /^\s*\[([^\]]+)\]\s*(#.*)?$/.exec(line);
      if (header) {
        inSection = header[1].trim() === wanted;
        continue;
      }
      if (!inSection) continue;
      const version = /^\s*version\s*=\s*["']([^"']+)["']/.exec(line);
      if (version) return version[1];
    }
  }
  return null;
}

/** リポジトリのルートを調べ、バージョンを返す。食い違いがあれば UsageError */
export function checkVersion({ root = ".", tag } = {}) {
  const packagePath = join(root, "package.json");
  const version = readJson(packagePath).version;
  if (typeof version !== "string" || version === "") {
    throw new UsageError(`${packagePath} に version がありません`);
  }

  const problems = [];

  const cargoPath = join(root, "Cargo.toml");
  const cargo = cargoVersion(readText(cargoPath));
  if (cargo === null) {
    problems.push(`${cargoPath} の [workspace.package] に version がありません`);
  } else if (cargo !== version) {
    problems.push(`${cargoPath} のバージョンは ${cargo} です`);
  }

  // tauri.conf.json の version は、値そのものか、package.json へのパス。
  // 省略時は Cargo.toml の値が使われる（上で確かめている）
  const tauriPath = join(root, "src-tauri", "tauri.conf.json");
  const configured = readJson(tauriPath).version;
  if (configured !== undefined) {
    const tauri =
      typeof configured === "string" && configured.endsWith(".json")
        ? readJson(resolve(dirname(tauriPath), configured)).version
        : configured;
    if (tauri !== version) {
      problems.push(`${tauriPath} のバージョンは ${tauri} です`);
    }
  }

  if (tag !== undefined && tag !== `v${version}`) {
    problems.push(`タグは ${tag} です（v${version} のはず）`);
  }

  if (problems.length > 0) {
    throw new UsageError(
      `バージョンが食い違っています。${packagePath} は ${version} ですが、\n` +
        problems.map((problem) => `- ${problem}`).join("\n"),
    );
  }
  return version;
}

function main(argv) {
  const options = readOptions(argv, { tag: {}, root: {} }, USAGE);
  const version = checkVersion({ root: options.root ?? ".", tag: options.tag });
  process.stdout.write(`${version}\n`);
}

if (isMain(import.meta.url)) await run(main);
