// テスト用: その場で使い捨ての鍵を作り、Tauri CLI と同じ形式の公開鍵・署名を組み立てる。
// （本番の署名は Tauri CLI が行う。ここはテストの入力を作るためだけのもの）

import { createHash, generateKeyPairSync, randomBytes, sign } from "node:crypto";

const toOuterBase64 = (text) => Buffer.from(text, "utf8").toString("base64");

/** 使い捨ての鍵を作る。pubkey は tauri.conf.json の plugins.updater.pubkey と同じ形式 */
export function createTestKey() {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const raw = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
  const keyId = randomBytes(8);
  const body = Buffer.concat([Buffer.from("Ed", "latin1"), keyId, raw]).toString("base64");
  return {
    privateKey,
    keyId,
    pubkey: toOuterBase64(`untrusted comment: minisign public key: TEST\n${body}\n`),
  };
}

/**
 * data に署名し、`.sig` ファイルの中身と同じ形式の文字列を返す。
 * file・version は、Tauri CLI が trusted comment に書き込むファイル名とバージョン。
 */
export function signForTest(key, data, { file, version } = {}) {
  let trustedComment = "timestamp:1700000000";
  if (file !== undefined) trustedComment += `\tfile:${file}`;
  if (version !== undefined) trustedComment += `\tversion:${version}`;
  const signature = sign(
    null,
    createHash("blake2b512").update(data).digest(),
    key.privateKey,
  );
  const body = Buffer.concat([Buffer.from("ED", "latin1"), key.keyId, signature]);
  const globalSignature = sign(
    null,
    Buffer.concat([signature, Buffer.from(trustedComment, "utf8")]),
    key.privateKey,
  );
  return toOuterBase64(
    [
      "untrusted comment: signature from tauri secret key",
      body.toString("base64"),
      `trusted comment: ${trustedComment}`,
      globalSignature.toString("base64"),
      "",
    ].join("\n"),
  );
}
