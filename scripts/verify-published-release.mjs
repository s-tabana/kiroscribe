#!/usr/bin/env node
// 公開した直後のリリースを、利用者のアプリと同じ道筋でたどって確かめる。
//
// 使い方（release.yml が、公開の直後に実行する）:
//   node scripts/verify-published-release.mjs \
//     --repo s-tabana/kiroscribe --version 0.4.0 \
//     --installer target/release/bundle/nsis/Kiroscribe_0.4.0_x64-setup.exe \
//     --sums release-assets/SHA256SUMS.txt \
//     --config src-tauri/tauri.conf.json \
//     [--previous-config <1 つ前のリリースの tauri.conf.json>] [--prerelease] [--timeout-seconds 600]
//
// 確かめること:
// 1. このリリースに添付した latest.json・SHA256SUMS.txt・.sig が、ビルドしたものと同じ
// 2. 利用者のアプリが見る latest.json（…/releases/latest/download/latest.json）が、このバージョンを指している
//    （プレリリースなら、指していない。プレリリースは利用者のアプリに配らない）
// 3. latest.json に書かれた URL から取得したインストーラが、ビルドしたものと同じ（SHA-256）
// 4. latest.json の署名が、そのインストーラに対して、アプリに埋め込む公開鍵と、1 つ前のリリースに埋め込まれていた
//    公開鍵（利用者のアプリが信頼している鍵）の両方で検証できる
//
// 公開の直後は、配信（とくに「最新のリリース」への転送）が追いつくのに時間がかかることがあるので、
// 合わないときは --timeout-seconds（既定 600 秒）のあいだ確かめ直す。同じタグで実行し直した直後は、配信の
// キャッシュに新旧のファイルが混ざることがあるので、公開したファイルどうしの食い違い（.sig と latest.json の
// 署名、署名の検証）も確かめ直す。

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { UsageError, isMain, readOptions, run } from "./lib/cli.mjs";
import { formatKeyId, verifySignature } from "./lib/minisign.mjs";
import { readPublicKey } from "./verify-update-signature.mjs";

const USAGE =
  "node scripts/verify-published-release.mjs --repo <owner/name> --version <バージョン> --installer <インストーラ> --sums <SHA256SUMS.txt> --config <tauri.conf.json> [--previous-config <tauri.conf.json>] [--prerelease] [--timeout-seconds 600]";

// updater が Windows（64 ビット）で探すキー（make-latest-json.mjs と同じ）
const WINDOWS_TARGET = "windows-x86_64";

const sha256 = (data) => createHash("sha256").update(data).digest("hex");

/** SHA256SUMS.txt（「<SHA-256>  <ファイル名>」の行）から、ファイル名ごとの SHA-256 を取り出す */
export function parseSums(text) {
  const sums = new Map();
  for (const line of text.split(/\r?\n/)) {
    const match = /^([0-9a-f]{64}) [ *](.+)$/.exec(line.trim());
    if (match) sums.set(match[2], match[1]);
  }
  return sums;
}

/** 合わなかった理由（確かめ直す価値がある、つまり配信が追いついていない・キャッシュに新旧が混ざっているだけかもしれないもの） */
class NotYet extends Error {}

async function get(fetchImpl, url, { as = "text" } = {}) {
  let response;
  try {
    response = await fetchImpl(url, { redirect: "follow" });
  } catch (error) {
    throw new NotYet(`${url} を取得できません: ${error.message}`);
  }
  if (!response.ok) throw new NotYet(`${url} が HTTP ${response.status} を返しました`);
  return as === "bytes" ? Buffer.from(await response.arrayBuffer()) : response.text();
}

function parseLatest(text, url) {
  let latest;
  try {
    latest = JSON.parse(text);
  } catch (error) {
    throw new NotYet(`${url} が JSON として読めません: ${error.message}`);
  }
  const platform = latest?.platforms?.[WINDOWS_TARGET];
  if (typeof latest?.version !== "string" || typeof platform?.url !== "string" || typeof platform?.signature !== "string") {
    throw new NotYet(`${url} に version と platforms.${WINDOWS_TARGET} の url・signature がありません`);
  }
  return latest;
}

/**
 * 公開したリリースを確かめる。問題がなければ、確かめた内容の要約（文字列の配列）を返す。
 * 問題が続けば UsageError を投げる。
 * options: { repo, version, installer, sums, pubkeys: [{ label, pubkey }], prerelease, baseUrl, timeoutMs, intervalMs }
 * 2 つ目の引数: { fetch, sleep, log }（テストで差し替える。log は、確かめ直すときの知らせ）
 */
export async function verifyPublishedRelease(options, { fetch: fetchImpl = globalThis.fetch, sleep, log = console.log } = {}) {
  const { repo, version, prerelease = false, baseUrl = "https://github.com", timeoutMs = 600_000, intervalMs = 10_000 } =
    options;
  const wait = sleep ?? ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
  const name = basename(options.installer);
  const installer = readFileSync(options.installer);
  const localHash = sha256(installer);
  const localSums = readFileSync(options.sums, "utf8");

  // ビルドしたファイル同士の食い違いは、待っても直らないので、すぐ止める
  const listed = parseSums(localSums).get(name);
  if (listed !== localHash) {
    throw new UsageError(
      `SHA256SUMS.txt の ${name} の値（${listed ?? "なし"}）が、ビルドしたインストーラの SHA-256（${localHash}）と違います`,
    );
  }

  const download = `${baseUrl}/${repo}/releases/download/v${version}`;
  const expectedUrl = `${download}/${name}`;
  const latestUrl = `${baseUrl}/${repo}/releases/latest/download/latest.json`;

  const attempt = async () => {
    const report = [];
    // 1. このリリースに添付したもの
    const own = parseLatest(await get(fetchImpl, `${download}/latest.json`), `${download}/latest.json`);
    if (own.version !== version) {
      throw new UsageError(`このリリースの latest.json の version が「${own.version}」です（${version} のはず）`);
    }
    if (own.platforms[WINDOWS_TARGET].url !== expectedUrl) {
      throw new UsageError(
        `このリリースの latest.json の url が「${own.platforms[WINDOWS_TARGET].url}」です（${expectedUrl} のはず）`,
      );
    }
    const publishedSums = await get(fetchImpl, `${download}/SHA256SUMS.txt`);
    if (publishedSums.replace(/\r\n/g, "\n").trim() !== localSums.replace(/\r\n/g, "\n").trim()) {
      throw new NotYet("このリリースの SHA256SUMS.txt が、ビルドのときに作ったものと違います");
    }
    const publishedSig = (await get(fetchImpl, `${expectedUrl}.sig`)).trim();
    if (publishedSig !== own.platforms[WINDOWS_TARGET].signature.trim()) {
      throw new NotYet("このリリースの .sig と、latest.json の signature が違います");
    }
    report.push(`このリリースの latest.json・SHA256SUMS.txt・.sig: ${download}/`);

    // 2. 利用者のアプリが見る latest.json
    const seen = parseLatest(await get(fetchImpl, latestUrl), latestUrl);
    if (prerelease) {
      if (seen.version === version) {
        throw new UsageError(`プレリリースの ${version} が、利用者のアプリが見る latest.json（${latestUrl}）に出ています`);
      }
      report.push(`利用者のアプリが見る latest.json は ${seen.version} のまま（プレリリースは配らない）`);
    } else {
      if (seen.version !== version) {
        throw new NotYet(`利用者のアプリが見る latest.json（${latestUrl}）が、まだ ${seen.version} を指しています`);
      }
      if (JSON.stringify(seen) !== JSON.stringify(own)) {
        throw new NotYet(`利用者のアプリが見る latest.json（${latestUrl}）が、このリリースのものと違います`);
      }
      report.push(`利用者のアプリが見る latest.json は ${version}: ${latestUrl}`);
    }

    // 3. latest.json の URL から取得したインストーラ
    const fetched = await get(fetchImpl, own.platforms[WINDOWS_TARGET].url, { as: "bytes" });
    const fetchedHash = sha256(fetched);
    if (fetchedHash !== localHash) {
      throw new NotYet(`latest.json の URL から取得したインストーラの SHA-256（${fetchedHash}）が、ビルドしたもの（${localHash}）と違います`);
    }
    report.push(`インストーラの SHA-256: ${localHash}（${fetched.length} バイト）`);

    // 4. 署名を、それぞれの公開鍵で
    for (const { label, pubkey } of options.pubkeys) {
      let sig;
      try {
        sig = verifySignature(fetched, own.platforms[WINDOWS_TARGET].signature, pubkey);
      } catch (error) {
        throw new NotYet(`latest.json の署名を、${label}で検証できません: ${error.message}`);
      }
      report.push(`署名を${label}で確認しました（鍵 ID ${formatKeyId(sig.keyId)}）`);
    }
    return report;
  };

  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      return await attempt();
    } catch (error) {
      if (!(error instanceof NotYet)) throw error;
      if (Date.now() >= deadline) {
        throw new UsageError(`公開したリリースを確かめられませんでした（${Math.round(timeoutMs / 1000)} 秒待ちました）: ${error.message}`);
      }
      log(`まだ確かめられません。${Math.round(intervalMs / 1000)} 秒後に確かめ直します: ${error.message}`);
      await wait(intervalMs);
    }
  }
}

async function main(argv) {
  const options = readOptions(
    argv,
    {
      repo: { required: true },
      version: { required: true },
      installer: { required: true },
      sums: { required: true },
      config: { required: true },
      "previous-config": {},
      prerelease: { flag: true },
      "timeout-seconds": {},
      "base-url": {},
    },
    USAGE,
  );
  const pubkeys = [{ label: "アプリに埋め込む公開鍵", pubkey: readPublicKey(options.config) }];
  if (options["previous-config"]) {
    pubkeys.push({ label: "1 つ前のリリースの公開鍵", pubkey: readPublicKey(options["previous-config"]) });
  }
  const timeoutSeconds = Number(options["timeout-seconds"] ?? 600);
  if (!Number.isFinite(timeoutSeconds) || timeoutSeconds < 0) {
    throw new UsageError(`--timeout-seconds「${options["timeout-seconds"]}」が 0 以上の数ではありません`);
  }
  const report = await verifyPublishedRelease({
    repo: options.repo,
    version: options.version,
    installer: options.installer,
    sums: options.sums,
    pubkeys,
    prerelease: options.prerelease ?? false,
    baseUrl: options["base-url"] ?? "https://github.com",
    timeoutMs: timeoutSeconds * 1000,
  });
  console.log(`公開したリリース ${options.version} を確かめました:\n${report.map((line) => `- ${line}`).join("\n")}`);
}

if (isMain(import.meta.url)) await run(main);
