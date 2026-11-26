import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { createTestKey, signForTest } from "./lib/minisign-fixture.mjs";
import { parseSums, verifyPublishedRelease } from "./verify-published-release.mjs";

const REPO = "owner/app";
const VERSION = "1.2.0";
const PREVIOUS = "1.1.0";
const NAME = `Kiroscribe_${VERSION}_x64-setup.exe`;
const INSTALLER = Buffer.from("本物のインストーラの代わり\n".repeat(100));
const sha256 = (data) => createHash("sha256").update(data).digest("hex");

/**
 * GitHub のリリースの配布と同じ URL を返す、手元のサーバー。
 * routes の中身を書き換えて、公開の途中・食い違いの状態を作る。
 * インストーラは、GitHub と同じく別の場所へ転送して返す
 */
let server;
let baseUrl;
let routes;
const requested = [];
before(async () => {
  server = createServer((req, res) => {
    requested.push(req.url);
    if (req.url.startsWith("/storage/")) {
      const body = routes.storage.get(req.url);
      res.writeHead(body ? 200 : 404).end(body);
      return;
    }
    const route = routes.pages.get(req.url);
    const body = typeof route === "function" ? route() : route;
    if (body === undefined) {
      res.writeHead(404).end("Not Found");
    } else if (typeof body === "object" && body.redirect) {
      res.writeHead(302, { location: body.redirect }).end();
    } else {
      res.writeHead(200).end(body);
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  baseUrl = `http://127.0.0.1:${server.address().port}`;
});
after(() => server.close());

const key = createTestKey();
const otherKey = createTestKey();

function latestJson(version, signature, file = `Kiroscribe_${version}_x64-setup.exe`) {
  return JSON.stringify({
    version,
    notes: "- 変更点",
    pub_date: "2026-12-01T00:00:00Z",
    platforms: {
      "windows-x86_64": { signature, url: `${baseUrl}/${REPO}/releases/download/v${version}/${file}` },
    },
  });
}

/** 正しく公開できた状態の配布。overrides で一部を差し替える */
function publish({ latest, installer = INSTALLER, signedBy = key, sums } = {}) {
  const signature = signForTest(signedBy, INSTALLER, { file: NAME, version: VERSION });
  const own = latestJson(VERSION, signature);
  const download = `/${REPO}/releases/download/v${VERSION}`;
  routes = {
    pages: new Map([
      [`${download}/latest.json`, own],
      [`${download}/SHA256SUMS.txt`, sums ?? `${sha256(INSTALLER)}  ${NAME}\n`],
      [`${download}/${NAME}.sig`, signature],
      [`${download}/${NAME}`, { redirect: `${baseUrl}/storage/${NAME}` }],
      [`/${REPO}/releases/latest/download/latest.json`, latest ?? own],
    ]),
    storage: new Map([[`/storage/${NAME}`, installer]]),
  };
  return { own, signature };
}

let dir;
before(() => {
  dir = mkdtempSync(join(tmpdir(), "verify-published-"));
  writeFileSync(join(dir, NAME), INSTALLER);
  writeFileSync(join(dir, "SHA256SUMS.txt"), `${sha256(INSTALLER)}  ${NAME}\n`);
});
after(() => rmSync(dir, { recursive: true, force: true }));

function verify(options = {}) {
  return verifyPublishedRelease(
    {
      repo: REPO,
      version: VERSION,
      installer: join(dir, NAME),
      sums: join(dir, "SHA256SUMS.txt"),
      pubkeys: [
        { label: "アプリに埋め込む公開鍵", pubkey: key.pubkey },
        { label: "1 つ前のリリースの公開鍵", pubkey: key.pubkey },
      ],
      baseUrl,
      timeoutMs: 0,
      intervalMs: 0,
      ...options,
    },
    // テストの実行の仕組みは、子のプロセスの標準出力でやり取りするので、テストの中では何も出さない
    { sleep: async () => {}, log: () => {} },
  );
}

test("[REL-008][REL-012] 利用者のアプリと同じ道筋で、latest.json・インストーラ・署名・SHA-256 を確かめる", async () => {
  publish();
  requested.length = 0;
  const report = await verify();
  assert.ok(report.some((line) => line.includes(`latest.json は ${VERSION}`)), report.join("\n"));
  assert.ok(report.some((line) => line.includes(sha256(INSTALLER))), report.join("\n"));
  assert.equal(report.filter((line) => line.startsWith("署名を")).length, 2);
  // インストーラは、latest.json に書かれた URL から（転送をたどって）取得している
  assert.ok(requested.includes(`/${REPO}/releases/download/v${VERSION}/${NAME}`));
  assert.ok(requested.includes(`/storage/${NAME}`));
  assert.ok(requested.includes(`/${REPO}/releases/latest/download/latest.json`));
});

test("[REL-012] 利用者のアプリが見る latest.json が古いままなら、確かめ直し、追いつけば通す", async () => {
  const { own } = publish();
  const stale = latestJson(PREVIOUS, signForTest(key, Buffer.from("前の版"), { file: "x", version: PREVIOUS }));
  let calls = 0;
  routes.pages.set(`/${REPO}/releases/latest/download/latest.json`, () => (++calls <= 2 ? stale : own));
  const report = await verify({ timeoutMs: 60_000 });
  assert.equal(calls, 3);
  assert.ok(report.some((line) => line.includes(`latest.json は ${VERSION}`)));
});

test("[REL-012] 時間内に追いつかなければ、理由を書いて失敗にする", async () => {
  const stale = latestJson(PREVIOUS, signForTest(key, Buffer.from("前の版"), { file: "x", version: PREVIOUS }));
  publish({ latest: stale });
  await assert.rejects(verify(), /利用者のアプリが見る latest\.json.*まだ 1\.1\.0 を指しています/);
});

test("[REL-012] 配られるインストーラが、ビルドしたものと 1 バイトでも違えば失敗にする", async () => {
  publish({ installer: Buffer.concat([INSTALLER, Buffer.from("x")]) });
  await assert.rejects(verify(), /SHA-256.*ビルドしたもの.*と違います/);
});

test("[REL-012] 1 つ前のリリースの公開鍵（利用者のアプリが信頼している鍵）で検証できなければ失敗にする", async () => {
  publish();
  await assert.rejects(
    verify({
      pubkeys: [
        { label: "アプリに埋め込む公開鍵", pubkey: key.pubkey },
        { label: "1 つ前のリリースの公開鍵", pubkey: otherKey.pubkey },
      ],
    }),
    /1 つ前のリリースの公開鍵で検証できません/,
  );
});

test("[REL-012] 公開した SHA256SUMS.txt や .sig がビルドのときと違えば失敗にする", async () => {
  publish({ sums: `${"0".repeat(64)}  ${NAME}\n` });
  await assert.rejects(verify(), /SHA256SUMS\.txt が、ビルドのときに作ったものと違います/);

  publish();
  routes.pages.set(`/${REPO}/releases/download/v${VERSION}/${NAME}.sig`, "別の署名");
  await assert.rejects(verify(), /\.sig と、latest\.json の signature が違います/);
});

test("[REL-012] 公開したファイルどうしが一時的に食い違っても（配信のキャッシュに新旧が混ざる）、確かめ直し、そろえば通す", async () => {
  // .sig だけが古い
  const { signature } = publish();
  let sigCalls = 0;
  routes.pages.set(`/${REPO}/releases/download/v${VERSION}/${NAME}.sig`, () => (++sigCalls <= 1 ? "古い署名" : signature));
  await verify({ timeoutMs: 60_000 });
  assert.equal(sigCalls, 2);

  // latest.json と .sig が、同じタグで実行し直す前のもの（別の鍵の署名）のまま
  const { own } = publish();
  const oldSignature = signForTest(otherKey, INSTALLER, { file: NAME, version: VERSION });
  const old = latestJson(VERSION, oldSignature);
  const download = `/${REPO}/releases/download/v${VERSION}`;
  let ownCalls = 0;
  const current = () => ownCalls <= 1;
  routes.pages.set(`${download}/latest.json`, () => (++ownCalls <= 1 ? old : own));
  routes.pages.set(`${download}/${NAME}.sig`, () => (current() ? oldSignature : signature));
  routes.pages.set(`/${REPO}/releases/latest/download/latest.json`, () => (current() ? old : own));
  const report = await verify({ timeoutMs: 60_000 });
  assert.equal(ownCalls, 2);
  assert.equal(report.filter((line) => line.startsWith("署名を")).length, 2);
});

test("[REL-012] 公開したファイルどうしの食い違いが時間内に解けなければ、理由を書いて失敗にする", async () => {
  publish({ signedBy: otherKey });
  await assert.rejects(verify(), /確かめられませんでした.*アプリに埋め込む公開鍵で検証できません/);
});

test("[REL-012] 手元の SHA256SUMS.txt がビルドしたインストーラと合わなければ、配布を見る前に止める", async () => {
  publish();
  const sums = join(dir, "wrong-sums.txt");
  writeFileSync(sums, `${"1".repeat(64)}  ${NAME}\n`);
  requested.length = 0;
  await assert.rejects(verify({ sums }), /SHA256SUMS\.txt の .* の値/);
  assert.equal(requested.length, 0);
});

test("[REL-008][REL-012] プレリリースは、利用者のアプリが見る latest.json に出ていないことを確かめる", async () => {
  const stale = latestJson(PREVIOUS, signForTest(key, Buffer.from("前の版"), { file: "x", version: PREVIOUS }));
  publish({ latest: stale });
  const report = await verify({ prerelease: true });
  assert.ok(report.some((line) => line.includes(`${PREVIOUS} のまま`)), report.join("\n"));

  publish();
  await assert.rejects(verify({ prerelease: true }), /プレリリースの 1\.2\.0 が、利用者のアプリが見る latest\.json/);
});

test("SHA256SUMS.txt の行を読む（sha256sum の 2 つの書き方）", () => {
  const hash = "a".repeat(64);
  const sums = parseSums(`${hash}  one.exe\r\n${hash} *two.exe\nこわれた行\n`);
  assert.equal(sums.get("one.exe"), hash);
  assert.equal(sums.get("two.exe"), hash);
  assert.equal(sums.size, 2);
});
