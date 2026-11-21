import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createTestKey, signForTest } from "./lib/minisign-fixture.mjs";
import { formatKeyId, parsePublicKey, parseSignature, verifySignature } from "./lib/minisign.mjs";

const SCRIPT = fileURLToPath(new URL("./verify-update-signature.mjs", import.meta.url));

// 本物の Tauri CLI（2.12.6）の出力。使い捨ての鍵を `tauri signer generate` で作り、
// 下の DATA を書いたファイルに `tauri signer sign` で署名したもの（秘密鍵は残していない）。
// 自前の実装が、Tauri の書き出す形式を実際に検証できることを確かめるために置いている
const REAL = {
  data: Buffer.from("Kiroscribe update signature fixture\n"),
  pubkey:
    "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEEyMkJDMDY1REIzQkQ3OEIKUldTTDF6dmJaY0Fyb2p6LzhXNlFKM0Z4Q2I2N0drcnJlL1poZDREMXdwY0dUUjQyNW1kbHd5RkIK",
  signature:
    "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVTTDF6dmJaY0Fyb2xYSE9ESWhQY2xOYkxQN201NXpnZ3ZvOHpFU3hWZTVqS0dJZEtrSGlJd2kzaEcvUXBONnU1TUtsNDkwbFlKdXo1NXV5WktvY3hGb2lyNXZGTlpsVXdVPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk1MjUwNzgzCWZpbGU6S2lyb3NjcmliZV8xLjIuM194NjQtc2V0dXAuZXhlCmlvTkZxc3JFMTNJbGxnUDc1dU5kVEVzZnV4cTZaSTJrNTFrZjNkdmZSVTRYbjdKdmtzaVlLOS92dmg2bjk3Z0x2dU5kY0tMS2duQno1OHpxekdjQURBPT0K",
  keyId: "A22BC065DB3BD78B",
};

test("Tauri CLI が実際に書き出した署名を、対になる公開鍵で検証できる", () => {
  const sig = verifySignature(REAL.data, REAL.signature, REAL.pubkey);
  assert.equal(sig.algorithm, "ED");
  assert.equal(formatKeyId(sig.keyId), REAL.keyId);
  assert.equal(formatKeyId(parsePublicKey(REAL.pubkey).keyId), REAL.keyId);
  assert.equal(sig.fields.file, "Kiroscribe_1.2.3_x64-setup.exe");
  assert.match(sig.fields.timestamp, /^\d+$/);
});

test("Tauri CLI の署名でも、ファイルが 1 バイトでも違えば検証に失敗する", () => {
  const changed = Buffer.concat([REAL.data, Buffer.from("x")]);
  assert.throws(() => verifySignature(changed, REAL.signature, REAL.pubkey), /ファイルの中身と合いません/);
});

test("ビルド時の署名と同じく、ファイル名とバージョンを読み取れる", () => {
  const key = createTestKey();
  const data = Buffer.from("installer bytes");
  const sigText = signForTest(key, data, { file: "Kiroscribe_0.2.0_x64-setup.exe", version: "0.2.0" });
  const sig = verifySignature(data, sigText, key.pubkey);
  assert.deepEqual(sig.fields, {
    timestamp: "1700000000",
    file: "Kiroscribe_0.2.0_x64-setup.exe",
    version: "0.2.0",
  });
  assert.deepEqual(parseSignature(`${sigText}\r\n`).fields, sig.fields);
});

test("別の鍵で署名されていれば、鍵の取り違えとして知らせる", () => {
  const signer = createTestKey();
  const embedded = createTestKey();
  const data = Buffer.from("installer bytes");
  assert.throws(
    () => verifySignature(data, signForTest(signer, data), embedded.pubkey),
    /対になっていません.*TAURI_SIGNING_PRIVATE_KEY/s,
  );
});

test("鍵 ID だけが同じで鍵が違う場合も、検証に失敗する", () => {
  const signer = createTestKey();
  const other = createTestKey();
  const data = Buffer.from("installer bytes");
  // 別の鍵の公開鍵に、署名した鍵の ID を付けたものを作る
  const raw = Buffer.from(
    Buffer.from(other.pubkey, "base64").toString("utf8").trim().split("\n").at(-1),
    "base64",
  );
  signer.keyId.copy(raw, 2);
  const forgedPubkey = Buffer.from(`untrusted comment: x\n${raw.toString("base64")}\n`).toString("base64");
  assert.throws(
    () => verifySignature(data, signForTest(signer, data), forgedPubkey),
    /ファイルの中身と合いません/,
  );
});

test("trusted comment（ファイル名・バージョン）が書き換えられていれば失敗する", () => {
  const key = createTestKey();
  const data = Buffer.from("installer bytes");
  const sigText = signForTest(key, data, { file: "a.exe", version: "0.1.0" });
  const tampered = Buffer.from(
    Buffer.from(sigText, "base64").toString("utf8").replace("version:0.1.0", "version:9.9.9"),
  ).toString("base64");
  assert.throws(() => verifySignature(data, tampered, key.pubkey), /trusted comment が改変/);
});

test("形式の違う入力は、分かるメッセージで断る", () => {
  const key = createTestKey();
  const data = Buffer.from("x");
  assert.throws(() => verifySignature(data, "", key.pubkey), /署名.*base64/);
  assert.throws(() => verifySignature(data, signForTest(key, data), "これは鍵ではない"), /公開鍵.*base64/);
  assert.throws(
    () => verifySignature(data, signForTest(key, data), Buffer.from("x\ny\n").toString("base64")),
    /公開鍵.*minisign/,
  );
});

test("コマンドとして実行すると、tauri.conf.json の公開鍵で検証する", () => {
  const dir = mkdtempSync(join(tmpdir(), "kiroscribe-verify-"));
  try {
    const key = createTestKey();
    const file = join(dir, "Kiroscribe_0.2.0_x64-setup.exe");
    const data = Buffer.from("installer bytes");
    writeFileSync(file, data);
    writeFileSync(`${file}.sig`, signForTest(key, data, { file: "Kiroscribe_0.2.0_x64-setup.exe", version: "0.2.0" }));
    const config = join(dir, "tauri.conf.json");
    writeFileSync(config, JSON.stringify({ plugins: { updater: { pubkey: key.pubkey } } }));
    const run = (configPath) =>
      spawnSync(
        process.execPath,
        [SCRIPT, "--file", file, "--signature", `${file}.sig`, "--config", configPath],
        { encoding: "utf8", env: { ...process.env, GITHUB_ACTIONS: "" } },
      );

    const ok = run(config);
    assert.equal(ok.status, 0, ok.stderr);
    assert.match(ok.stdout, /署名を確認しました/);

    const otherConfig = join(dir, "other.conf.json");
    writeFileSync(otherConfig, JSON.stringify({ plugins: { updater: { pubkey: createTestKey().pubkey } } }));
    const mismatch = run(otherConfig);
    assert.equal(mismatch.status, 1);
    assert.match(mismatch.stderr, /エラー: 署名に使われた鍵.*対になっていません/);

    const noKeyConfig = join(dir, "nokey.conf.json");
    writeFileSync(noKeyConfig, JSON.stringify({ bundle: {} }));
    const noKey = run(noKeyConfig);
    assert.equal(noKey.status, 1);
    assert.match(noKey.stderr, /plugins\.updater\.pubkey がありません/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
