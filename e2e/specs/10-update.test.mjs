// アプリ内更新: 新しい版の知らせ、ダウンロード、署名の確認。
// 配布元は模擬サーバー、署名はテストのたびに作る使い捨ての鍵（e2e/lib/update-fixture.mjs）。

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before } from "node:test";
import { ROOT, test } from "../lib/app.mjs";
import { closeSettings, openSettings, saveSettings, sel, settingsCheck } from "../lib/ui.mjs";
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

const AUTO_UPDATE = "新しいバージョンを自動でインストールする";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** アプリが信頼する公開鍵を、アプリの設定（config.json）に足す */
function trustKey(ctx, key) {
  ctx.work.configure({ ...JSON.parse(fs.readFileSync(ctx.work.file("config.json"), "utf8")), updatePubkey: key.pubkey });
}

/** 設定画面で「新しいバージョンを自動でインストールする」を切り替えて保存する */
async function setAutoUpdate(session, enabled) {
  await openSettings(session);
  const check = await session.$(settingsCheck(AUTO_UPDATE));
  if ((await check.isSelected()) !== enabled) await check.click();
  await saveSettings(session);
  await closeSettings(session);
}

const downloads = (ctx) => ctx.server.requestsTo("/update/files/").length;

/** サイドバーの知らせから更新を始める（確認には「はい」と答える） */
async function startUpdate(ctx, session) {
  ctx.work.answerDialogs({ confirm: true });
  await session.waitForText(sel.updateBanner, `新しいバージョン ${NEW_VERSION} があります`, { timeout: 30_000 });
  await session.click(`${sel.updateBanner} button`);
}

test("[UPD-004][UPD-007][UPD-015][UPD-020] 新しい版があると知らせが出て、［更新］でダウンロードと署名の確認まで進む", async (ctx) => {
  const update = publishUpdate(ctx.server, ctx.work.dir, { version: NEW_VERSION, signedBy: trusted, notes: "- 新しい機能" });
  trustKey(ctx, trusted);
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

test("[UPD-010][SEC-010] 信頼していない鍵で署名された更新は、ダウンロードしても適用しない", async (ctx) => {
  publishUpdate(ctx.server, ctx.work.dir, { version: NEW_VERSION, signedBy: untrusted });
  trustKey(ctx, trusted);
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

test("[UPD-006][SET-011] 更新を手動で確認できる: 最新なら「最新のバージョンです」、配布元に届かなければ理由が出る", async (ctx) => {
  const current = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).version;
  publishUpdate(ctx.server, ctx.work.dir, { version: current, signedBy: trusted });
  trustKey(ctx, trusted);
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

test("[UPD-021][UPD-022][UPD-023][UPD-026] 自動インストールをオンにすると、ダウンロードして署名を確かめておき、ウィンドウを表示している間は待つ。［今すぐ更新］で進み、オフにすると待つのをやめる", async (ctx) => {
  publishUpdate(ctx.server, ctx.work.dir, { version: NEW_VERSION, signedBy: trusted });
  trustKey(ctx, trusted);
  const session = await ctx.launch();

  // 最初はオフ: 知らせるだけで、ダウンロードしない
  await session.waitForText(sel.updateBanner, `新しいバージョン ${NEW_VERSION} があります`, { timeout: 30_000 });
  await sleep(1_000);
  assert.equal(downloads(ctx), 0, "自動インストールがオフなのに、ダウンロードしている");

  // オンにして保存すると、利用者の操作なしでダウンロードして、署名を確かめておく
  await setAutoUpdate(session, true);
  const ready = await session.waitForText(sel.updateBanner, "準備ができました", { timeout: 60_000 });
  assert.match(ready.replace(/\s+/g, ""), new RegExp(`新しいバージョン${NEW_VERSION.replaceAll(".", "\\.")}の準備ができました。ウィンドウを閉じると更新します`));
  await session.$(`xpath://section[@aria-label="アプリの更新"]//button[normalize-space(.)="今すぐ更新"]`);
  assert.equal(downloads(ctx), 1);
  await openSettings(session);
  await session.waitForText(`${sel.settingsDialog} .about-status`, "の準備ができました。ウィンドウを閉じると更新します");
  await session.$(`xpath://div[@aria-labelledby="settings-title"]//button[normalize-space(.)="今すぐ更新"]`);
  await closeSettings(session);

  // メインウィンドウを表示している間は、自動では入れない（アプリは動き続け、準備ができたままになる）
  await sleep(2_000);
  assert.equal(ctx.app.isRunning(), true);
  assert.ok((await session.text(sel.updateBanner)).includes("準備ができました"));
  assert.equal(await session.exists(bannerError), false);

  // オフに戻すと、待つのをやめて、知らせるだけの表示に戻る
  await setAutoUpdate(session, false);
  await session.waitForText(sel.updateBanner, `新しいバージョン ${NEW_VERSION} があります`);
  await session.$(`xpath://section[@aria-label="アプリの更新"]//button[normalize-space(.)="更新"]`);
  // もう一度オンにすると、ダウンロードからやり直す
  await setAutoUpdate(session, true);
  await session.waitForText(sel.updateBanner, "準備ができました", { timeout: 60_000 });
  assert.equal(downloads(ctx), 2, "オフにしたときに、ダウンロード済みのファイルを捨てていない");

  // ［今すぐ更新］: 確認のあと、ダウンロードし直さずにインストールへ進む
  // （このテストの更新ファイルは本物のインストーラではないので、インストールの段階で止まる）
  ctx.work.answerDialogs({ confirm: true });
  await session.click(`xpath://section[@aria-label="アプリの更新"]//button[normalize-space(.)="今すぐ更新"]`);
  const error = await session.waitForText(bannerError, "更新をインストールできませんでした", { timeout: 60_000 });
  assert.ok(!error.includes("署名"), error);
  assert.equal(
    ctx.work.dialogs().at(-1).message,
    `Kiroscribe ${NEW_VERSION} に更新します。\nアプリを終了し、新しいバージョンで起動し直します。`,
  );
  assert.equal(downloads(ctx), 2, "［今すぐ更新］で、ダウンロードし直している");
  assert.equal(ctx.app.isRunning(), true);
});

/** 自動インストールをオンにして保存し、アプリを止める（次の起動から、オンで始まる） */
async function enableAutoUpdateAndStop(ctx) {
  const session = await ctx.launch();
  await setAutoUpdate(session, true);
  await ctx.stop();
}

test("[UPD-021][UPD-022] 自動インストールがオンで、ウィンドウを閉じているあいだは、ダウンロードと署名の確認のあと、インストールへ進む", async (ctx) => {
  trustKey(ctx, trusted);
  await enableAutoUpdateAndStop(ctx);
  publishUpdate(ctx.server, ctx.work.dir, { version: NEW_VERSION, signedBy: trusted });

  // ウィンドウを出さずに起動する（タスクトレイに常駐している状態）
  const session = await ctx.launch({ args: ["--autostart"] });
  assert.equal(await ctx.app.isMainVisible(), false);
  await session.waitUntil(() => downloads(ctx) === 1, { timeout: 60_000, message: "利用者の操作なしでダウンロードする" });
  // 署名の確認を通って、インストールへ進む。このテストの更新ファイルは本物のインストーラではないので、
  // インストールの段階で止まる（そのことは、アプリのログにだけ出る）
  await session.waitUntil(() => ctx.work.logs().includes("更新を自動でインストールできませんでした"), {
    timeout: 60_000,
    message: "ウィンドウを閉じているあいだに、インストールへ進む",
  });
  assert.equal(ctx.app.isRunning(), true);
  assert.equal(await ctx.app.isMainVisible(), false, "自動の更新のために、ウィンドウを出している");

  // ウィンドウを開くと、新しいバージョンの知らせ（「更新」ボタン）は残っていて、失敗の知らせは出ていない
  await ctx.launchSecondInstance();
  await session.waitUntil(() => ctx.app.isMainVisible(), { message: "メインウィンドウが表示される" });
  await session.waitForText(sel.updateBanner, `新しいバージョン ${NEW_VERSION} があります`);
  assert.equal(await session.exists(bannerError), false, "自動の更新の失敗を、画面に知らせている");
  await sleep(1_000);
  assert.equal(downloads(ctx), 1, "失敗したあと、すぐに自動で試し直している");
});

test("[UPD-027][SEC-010] 自動インストールがオンでも、信頼していない鍵で署名された更新は入れず、そのことは画面に知らせない", async (ctx) => {
  trustKey(ctx, trusted);
  await enableAutoUpdateAndStop(ctx);
  publishUpdate(ctx.server, ctx.work.dir, { version: NEW_VERSION, signedBy: untrusted });

  const session = await ctx.launch({ args: ["--autostart"] });
  await session.waitUntil(() => downloads(ctx) === 1, { timeout: 60_000, message: "ダウンロードする" });
  await session.waitUntil(() => ctx.work.logs().includes("更新をダウンロードできませんでした"), {
    timeout: 60_000,
    message: "署名の確認で止まる",
  });
  assert.ok(!ctx.work.logs().includes("更新を自動でインストールできませんでした"), "署名が合わないのに、インストールへ進んでいる");
  assert.equal(ctx.app.isRunning(), true);

  await ctx.launchSecondInstance();
  await session.waitUntil(() => ctx.app.isMainVisible(), { message: "メインウィンドウが表示される" });
  const banner = await session.waitForText(sel.updateBanner, `新しいバージョン ${NEW_VERSION} があります`);
  assert.ok(!banner.includes("準備ができました"), banner);
  assert.equal(await session.exists(bannerError), false, "署名が合わなかったことを、画面に知らせている");
});

test("[UPD-024][UPD-025] 自動で更新したあとは、ウィンドウを出さずに起動し、次にウィンドウを開いたときに 1 回だけ知らせる", async (ctx) => {
  const current = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).version;
  /** 「前のバージョンから自動で更新して、起動し直すところ」の状態にする（アプリを止めているあいだに） */
  const markAutoUpdated = () =>
    ctx.work.withDatabase((db) => {
      const marker = JSON.stringify({ from: "0.0.1", to: current, at: Date.now() });
      db.prepare("INSERT OR REPLACE INTO settings (key, value) VALUES ('update.autoRestart', ?)").run(marker);
    });
  /** ウィンドウを開く（もう一度起動すると、動いているほうのウィンドウが出る） */
  const showMain = async (session) => {
    await ctx.launchSecondInstance();
    await session.waitUntil(() => ctx.app.isMainVisible(), { message: "メインウィンドウが表示される" });
  };

  // いちど起動してデータベースを作る
  await ctx.launch();
  await ctx.stop();
  await markAutoUpdated();

  // 自動の更新のあとの起動: ふつうの起動（--autostart なし）でも、ウィンドウを出さない
  let session = await ctx.launch();
  assert.equal(await ctx.app.isMainVisible(), false, "自動の更新のあとなのに、ウィンドウを出している");
  assert.equal(await session.exists(sel.updatedNotice), false);

  // ウィンドウを開くと、更新したことを知らせる
  await showMain(session);
  await session.waitForText(sel.updatedNotice, `バージョン ${current} に更新しました`);
  // ［リリースノート］で、ヘルプがリリースノートのタブで開き、知らせは消える
  await session.click(`${sel.updatedNotice} button.link-button`);
  await session.waitUntil(
    async () => (await session.text(`${sel.helpDialog} [role="tab"][aria-selected="true"]`)).trim() === "リリースノート",
    { message: "ヘルプがリリースノートのタブで開く" },
  );
  await session.click(`${sel.helpDialog} button[aria-label="閉じる"]`);
  await session.waitGone(sel.helpDialog);
  await session.waitGone(sel.updatedNotice);

  // 2 回目以降は表示しない（ウィンドウを閉じて開き直しても、起動し直しても）
  ctx.work.closeWindow("main");
  await session.waitUntil(async () => !(await ctx.app.isMainVisible()), { message: "メインウィンドウが見えなくなる" });
  await showMain(session);
  await sleep(1_000);
  assert.equal(await session.exists(sel.updatedNotice), false, "知らせを 2 回出している");
  session = await ctx.relaunch();
  assert.equal(await ctx.app.isMainVisible(), true, "2 回目の起動は、ふつうにウィンドウを出す");
  await sleep(1_000);
  assert.equal(await session.exists(sel.updatedNotice), false);

  // もう一度、自動で更新したあとの状態にする: 知らせは × でも閉じられる
  await ctx.stop();
  await markAutoUpdated();
  session = await ctx.launch();
  await showMain(session);
  await session.waitForText(sel.updatedNotice, `バージョン ${current} に更新しました`);
  await session.click('button[aria-label="更新の知らせを閉じる"]');
  await session.waitGone(sel.updatedNotice);
});
