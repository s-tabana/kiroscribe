// アプリ内更新: 新しい版の知らせ、ダウンロード、署名の確認。
// 配布元は模擬サーバー、署名はテストのたびに作る使い捨ての鍵（e2e/lib/update-fixture.mjs）。

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before } from "node:test";
import { ROOT, test } from "../lib/app.mjs";
import { openSettings, sel } from "../lib/ui.mjs";
import { createSigningKey, publishUpdate } from "../lib/update-fixture.mjs";
import { byText } from "../lib/webdriver.mjs";

const NEW_VERSION = "99.0.0";
const bannerError = `${sel.updateBanner} [role="alert"]`;

let keysDir;
/** アプリが信頼する鍵と、信頼しない別の鍵 */
let trusted;
let untrusted;
before(() => {
  keysDir = fs.mkdtempSync(path.join(os.tmpdir(), "kiroscribe-e2e-keys-"));
  trusted = createSigningKey(keysDir, "trusted");
  untrusted = createSigningKey(keysDir, "untrusted");
});
after(() => fs.rmSync(keysDir, { recursive: true, force: true }));

/** サイドバーの知らせから更新を始める（確認には「はい」と答える） */
async function startUpdate(ctx, session) {
  ctx.work.answerDialogs({ confirm: true });
  await session.waitForText(sel.updateBanner, `新しいバージョン ${NEW_VERSION} があります`, { timeout: 30_000 });
  await session.click(`${sel.updateBanner} button`);
}

test("新しい版があると知らせが出て、［更新］でダウンロードと署名の確認まで進む", async (ctx) => {
  const update = publishUpdate(ctx.server, ctx.work.dir, { version: NEW_VERSION, signedBy: trusted, notes: "- 新しい機能" });
  ctx.work.configure({ ...JSON.parse(fs.readFileSync(ctx.work.file("config.json"), "utf8")), updatePubkey: trusted.pubkey });
  const session = await ctx.launch();

  // 起動の少しあとに自動で確認して、知らせを出す（確認するだけで、ダウンロードはまだしない）
  await session.waitForText(sel.updateBanner, `新しいバージョン ${NEW_VERSION} があります`, { timeout: 30_000 });
  assert.equal(ctx.server.requestsTo("/update/files/").length, 0);

  // 設定画面にも出て、変更点を読める
  await openSettings(session);
  await session.waitForText(sel.settingsDialog, `新しいバージョン ${NEW_VERSION} があります`);
  await session.click(`${sel.settingsDialog} details summary`);
  await session.waitForText(`${sel.settingsDialog} details`, "新しい機能");
  await session.click(`xpath://div[@aria-labelledby="settings-title"]//footer//button[normalize-space(.)="閉じる"]`);
  await session.waitGone(sel.settingsDialog);

  // 確認で「いいえ」なら何もしない
  ctx.work.answerDialogs({ confirm: false });
  await session.click(`${sel.updateBanner} button`);
  await session.waitUntil(() => ctx.work.dialogs().length === 1, { message: "更新の確認が出る" });
  assert.match(ctx.work.dialogs()[0].message, new RegExp(`Kiroscribe ${NEW_VERSION.replaceAll(".", "\\.")} に更新します`));
  assert.equal(ctx.work.dialogs()[0].kind, "info");
  assert.equal(ctx.server.requestsTo("/update/files/").length, 0);

  // 「はい」→ ダウンロードして、署名を確かめる。
  // このテストの更新ファイルは本物のインストーラではないので、署名の確認を通ったあと、インストールの段階で止まる
  // （署名が合わないときのメッセージ（下のテスト）とは別のメッセージになる）
  await startUpdate(ctx, session);
  const error = await session.waitForText(bannerError, "更新をインストールできませんでした", { timeout: 60_000 });
  assert.ok(!error.includes("署名"), error);
  assert.equal(ctx.app.isRunning(), true);
  // 中止したあとも、同じ更新をもう一度試せる
  await session.waitForText(sel.updateBanner, `新しいバージョン ${NEW_VERSION} があります`);
  const downloads = ctx.server.requestsTo("/update/files/");
  assert.equal(downloads.length, 1);
  assert.equal(downloads[0].path, `/update/files/${update.fileName}`);
});

test("信頼していない鍵で署名された更新は、ダウンロードしても適用しない", async (ctx) => {
  publishUpdate(ctx.server, ctx.work.dir, { version: NEW_VERSION, signedBy: untrusted });
  ctx.work.configure({ ...JSON.parse(fs.readFileSync(ctx.work.file("config.json"), "utf8")), updatePubkey: trusted.pubkey });
  const session = await ctx.launch();

  await startUpdate(ctx, session);
  await session.waitForText(bannerError, "ダウンロードしたファイルの署名を確認できなかったため、更新を中止しました", {
    timeout: 60_000,
  });
  assert.equal(ctx.server.requestsTo("/update/files/").length, 1);
  assert.equal(ctx.app.isRunning(), true, "署名が合わないのにアプリが終了した");
  // 画面はそのまま使える
  await session.click(sel.newNote);
  await session.$(sel.title);
});

test("更新を手動で確認できる: 最新なら「最新のバージョンです」、配布元に届かなければ理由が出る", async (ctx) => {
  const current = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).version;
  publishUpdate(ctx.server, ctx.work.dir, { version: current, signedBy: trusted });
  ctx.work.configure({ ...JSON.parse(fs.readFileSync(ctx.work.file("config.json"), "utf8")), updatePubkey: trusted.pubkey });
  const session = await ctx.launch();

  await openSettings(session);
  await session.waitForText(sel.settingsDialog, `Kiroscribe ${current}`);
  await session.click(byText("button", "更新を確認"));
  await session.waitForText(`${sel.settingsDialog} .about-status`, "最新のバージョンです");
  assert.equal(await session.exists(sel.updateBanner), false);

  // 配布元にリリースがない（404）
  ctx.server.update.manifest = null;
  await session.click(byText("button", "更新を確認"));
  await session.waitForText(`${sel.settingsDialog} .about-status`, "更新の情報を取得できませんでした");
});
