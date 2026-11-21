// Tauri の更新用署名（minisign 形式）を読む・確かめるための道具。
//
// Tauri CLI が書き出す `.sig` と、tauri.conf.json の `plugins.updater.pubkey` は、
// どちらも minisign のテキスト全体をさらに base64 にしたもの。
//
//   署名のテキスト（4 行）:
//     untrusted comment: <コメント>
//     base64(アルゴリズム 2 バイト + 鍵 ID 8 バイト + 署名 64 バイト)
//     trusted comment: <コメント>            ← Tauri は「timestamp:..\tfile:..\tversion:..」を書く
//     base64(署名 + trusted comment に対する署名 64 バイト)
//
//   公開鍵のテキスト（2 行）:
//     untrusted comment: <コメント>
//     base64(アルゴリズム 2 バイト "Ed" + 鍵 ID 8 バイト + 公開鍵 32 バイト)

import { createHash, createPublicKey, verify } from "node:crypto";
import { UsageError } from "./cli.mjs";

const TRUSTED_PREFIX = "trusted comment: ";
// Ed25519 の公開鍵 32 バイトを、Node が読める形（SPKI の DER）にするための前置き
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

function decodeOuterBase64(text, what) {
  const compact = text.replace(/\s+/g, "");
  if (compact === "" || !/^[A-Za-z0-9+/]+={0,2}$/.test(compact)) {
    throw new UsageError(`${what}が base64 ではありません`);
  }
  return Buffer.from(compact, "base64").toString("utf8");
}

/** `.sig` ファイルの中身を読み解く */
export function parseSignature(sigText) {
  const lines = decodeOuterBase64(sigText, "署名（.sig の中身）").split(/\r?\n/);
  if (lines.length < 4 || !lines[2].startsWith(TRUSTED_PREFIX)) {
    throw new UsageError("署名（.sig の中身）が minisign の形式ではありません");
  }
  const body = Buffer.from(lines[1], "base64");
  const globalSignature = Buffer.from(lines[3], "base64");
  if (body.length !== 74 || globalSignature.length !== 64) {
    throw new UsageError("署名（.sig の中身）の長さが minisign の形式と合いません");
  }
  const trustedComment = lines[2].slice(TRUSTED_PREFIX.length);
  // Tauri CLI はタブ区切りの「キー:値」を書く（例: timestamp:1700000000\tfile:app.exe\tversion:1.2.3）
  const fields = {};
  for (const field of trustedComment.split("\t")) {
    const colon = field.indexOf(":");
    if (colon > 0) fields[field.slice(0, colon)] = field.slice(colon + 1);
  }
  return {
    algorithm: body.subarray(0, 2).toString("latin1"),
    keyId: body.subarray(2, 10),
    signature: body.subarray(10),
    trustedComment,
    globalSignature,
    fields,
  };
}

/** `plugins.updater.pubkey` の値を読み解く */
export function parsePublicKey(pubkeyText) {
  const lines = decodeOuterBase64(pubkeyText, "公開鍵（pubkey）")
    .split(/\r?\n/)
    .filter((line) => line !== "");
  const body = Buffer.from(lines.at(-1) ?? "", "base64");
  if (body.length !== 42 || body.subarray(0, 2).toString("latin1") !== "Ed") {
    throw new UsageError("公開鍵（pubkey）が minisign の形式ではありません");
  }
  return { keyId: body.subarray(2, 10), key: body.subarray(10) };
}

/** 鍵 ID を minisign と同じ表記（16 進の大文字）にする */
export function formatKeyId(keyId) {
  return Buffer.from(keyId).reverse().toString("hex").toUpperCase();
}

/**
 * data が、pubkeyText の公開鍵と対になる秘密鍵で署名されたものかを確かめる。
 * 問題があれば UsageError を投げ、問題がなければ読み解いた署名を返す。
 */
export function verifySignature(data, sigText, pubkeyText) {
  const sig = parseSignature(sigText);
  const pub = parsePublicKey(pubkeyText);
  if (!sig.keyId.equals(pub.keyId)) {
    throw new UsageError(
      `署名に使われた鍵（ID ${formatKeyId(sig.keyId)}）が、アプリに埋め込む公開鍵（ID ${formatKeyId(pub.keyId)}）と対になっていません。` +
        "Secret の TAURI_SIGNING_PRIVATE_KEY と tauri.conf.json の plugins.updater.pubkey の組み合わせを確かめてください",
    );
  }
  let message;
  if (sig.algorithm === "ED") {
    // 今の形式: ファイルそのものではなく、BLAKE2b-512 のハッシュに署名する
    message = createHash("blake2b512").update(data).digest();
  } else if (sig.algorithm === "Ed") {
    message = data;
  } else {
    throw new UsageError(`署名のアルゴリズム「${sig.algorithm}」には対応していません`);
  }
  const key = createPublicKey({
    key: Buffer.concat([ED25519_SPKI_PREFIX, pub.key]),
    format: "der",
    type: "spki",
  });
  if (!verify(null, message, key, sig.signature)) {
    throw new UsageError(
      "署名がファイルの中身と合いません（署名のあとでファイルが変わったか、別のファイルの署名です）",
    );
  }
  const signedComment = Buffer.concat([sig.signature, Buffer.from(sig.trustedComment, "utf8")]);
  if (!verify(null, signedComment, key, sig.globalSignature)) {
    throw new UsageError("署名の trusted comment が改変されています");
  }
  return sig;
}
