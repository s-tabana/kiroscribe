// 本物のインストーラを使う、インストールと更新の通しのテスト（Windows だけ）。
//
// e2e/installer/build.mjs で作った 2 つのインストーラ（古い版・新しい版。どちらも E2E テスト用のビルド）を使う。
// 1. 古い版を、利用者と同じインストーラで入れる
// 2. 入れたアプリを起動し、アプリ内更新で新しい版（模擬サーバーから配る、本物のインストーラ）に更新する
// 3. アプリが終了 → インストーラが入れ替え → 起動し直す、を確かめる（起動し直したアプリは WebDriver の外で動くので、
//    アプリが作業フォルダに残す起動の記録 launches.log で確かめる）
// 4. もう一度起動して、データが残っていること・新しい版になっていることを、画面で確かめる
//
// テストは 1 つのインストール先を順に使うので、この順に 1 件ずつ走らせる（node:test は、同じファイルの中では順に走らせる）。

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before } from "node:test";
import { test } from "../lib/app.mjs";
import { createNote, noteTitles, openSettings, saveSettings, closeSettings, sel, settingsCheck } from "../lib/ui.mjs";
import { createSigningKey, publishFile, signFile } from "../lib/update-fixture.mjs";
import { BUILD_INFO } from "./build.mjs";
import { fileVersion, install, installed, isAlive, shortcuts, uninstall, waitFor } from "./windows.mjs";

const build = JSON.parse(fs.readFileSync(BUILD_INFO, "utf8"));
const OLD = build.old.version;
const NEW = build.new.version;
const UPDATE_TIMEOUT = 15 * 60_000;
const bannerError = `${sel.updateBanner} [role="alert"]`;

/** アプリに作業フォルダを伝える引数。インストーラが起動し直したアプリにも引き継がれる（環境変数は引き継がれないことがある） */
const dirArg = (ctx) => `--kiroscribe-e2e-dir=${ctx.work.dir}`;

const sameFile = (a, b) => path.resolve(a).toLowerCase() === path.resolve(b).toLowerCase();

let keysDir;
let untrusted;
before(async () => {
  keysDir = fs.mkdtempSync(path.join(os.tmpdir(), "kiroscribe-installer-keys-"));
  untrusted = createSigningKey(keysDir, "untrusted");
  // 前の実行の残りがあれば消してから始める
  await uninstall();
});
after(async () => {
  await uninstall().catch((error) => console.warn(`後片付けのアンインストールに失敗しました: ${error.message}`));
  fs.rmSync(keysDir, { recursive: true, force: true });
});

/** インストール済みの古い版を起動する */
async function launchInstalled(ctx, { args = [] } = {}) {
  const info = installed();
  assert.ok(info?.exe, "アプリがインストールされていません");
  return ctx.launch({ binary: info.exe, args: [...args, dirArg(ctx)] });
}

/** 新しい版のインストーラを、模擬サーバーから更新として配る（signature を省くと、ビルドのときの署名） */
function publishNewVersion(ctx, { signature } = {}) {
  publishFile(ctx.server, {
    version: NEW,
    fileName: path.basename(build.new.installer),
    bytes: fs.readFileSync(build.new.installer),
    signature: signature ?? fs.readFileSync(build.new.signature, "utf8").trim(),
    notes: "- 本物のインストーラでの更新",
  });
}

/** サイドバーの知らせから、手動で更新を始める（確認には「はい」と答える） */
async function startUpdate(ctx, session) {
  ctx.work.answerDialogs({ confirm: true });
  await session.waitForText(sel.updateBanner, `新しいバージョン ${NEW} があります`, { timeout: 60_000 });
  await session.click(`${sel.updateBanner} button`);
}

/**
 * 更新のためにアプリが終了し、インストーラが新しい版を入れて、アプリを起動し直すまで待つ。
 * 起動し直したアプリの起動の記録を返す
 */
async function waitForUpdatedLaunch(ctx, { oldPid, launchesBefore }) {
  await waitFor(() => !isAlive(oldPid), { timeout: 5 * 60_000, message: `更新のために、古い版のアプリ（pid ${oldPid}）が終了する` });
  await waitFor(() => installed()?.displayVersion === NEW, {
    timeout: 5 * 60_000,
    message: `インストールの情報のバージョンが ${NEW} になる（いまは ${installed()?.displayVersion}）`,
  });
  return waitFor(() => ctx.work.launches().slice(launchesBefore).find((launch) => launch.version === NEW), {
    timeout: 3 * 60_000,
    message: `インストーラが、新しい版（${NEW}）のアプリを起動し直す`,
  });
}

test("[REL-001][REL-010] インストーラで入れると、アプリと機能の一覧・スタートメニューに出て、そのバージョンのアプリが起動する", async (ctx) => {
  const info = await install(build.old.installer);
  assert.equal(info.displayName, "Kiroscribe");
  assert.equal(info.displayVersion, OLD);
  assert.ok(info.exe && fs.existsSync(info.exe), `実行ファイルがありません: ${info.exe}`);
  assert.ok(sameFile(path.dirname(info.exe), path.join(process.env.LOCALAPPDATA, "Kiroscribe")), `利用者ごとの場所に入っていません: ${info.exe}`);
  assert.equal(fileVersion(info.exe), OLD);
  assert.ok(fs.existsSync(info.uninstaller), `アンインストーラがありません: ${info.uninstaller}`);
  assert.ok(fs.existsSync(shortcuts().startMenu), `スタートメニューのショートカットがありません: ${shortcuts().startMenu}`);

  const session = await launchInstalled(ctx);
  await openSettings(session);
  await session.waitForText(sel.settingsDialog, `Kiroscribe ${OLD}`);
  const launch = ctx.work.launches().at(-1);
  assert.equal(launch.version, OLD);
  assert.ok(sameFile(launch.exe, info.exe), launch.exe);
});

test("[UPD-010][SEC-010] 本物のインストーラでも、信頼していない鍵で署名された更新は実行せず、入っている版はそのまま", async (ctx) => {
  // 新しい版のインストーラに、アプリが信頼していない鍵で署名し直したものを配る
  const copy = path.join(ctx.work.dir, path.basename(build.new.installer));
  fs.copyFileSync(build.new.installer, copy);
  publishNewVersion(ctx, { signature: signFile(untrusted, copy, NEW) });

  const session = await launchInstalled(ctx);
  await startUpdate(ctx, session);
  await session.waitForText(bannerError, "ダウンロードしたファイルの署名を確認できなかったため、更新を中止しました", {
    timeout: 5 * 60_000,
  });
  assert.equal(ctx.server.requestsTo("/update/files/").length, 1);
  assert.equal(ctx.app.isRunning(), true, "署名が合わないのに、アプリが終了した");
  assert.equal(installed()?.displayVersion, OLD, "署名が合わないのに、インストーラが動いた");
  assert.equal(fileVersion(installed().exe), OLD);
  assert.equal(ctx.work.launches().length, 1, "アプリが起動し直した");
});

test("[UPD-012][UPD-013][UPD-029] ［更新］で、インストーラが新しい版を入れてアプリを起動し直し、ウィンドウを出す。データはそのまま残る", { timeout: UPDATE_TIMEOUT }, async (ctx) => {
  const info = installed();
  // 利用者がデスクトップのショートカットを消していても、更新で作り直さない
  fs.rmSync(shortcuts().desktop, { force: true });

  // ノートを作っておく
  let session = await launchInstalled(ctx);
  await createNote(session, { title: "更新の前に書いたメモ", memo: "更新のあとも残っているはず" });
  await ctx.stop();

  // Windows の起動時の自動起動と同じく、ウィンドウを出さずに起動し、トレイから開いたのと同じようにウィンドウを出す
  // （もう一度起動すると、動いているほうのウィンドウが出る）
  publishNewVersion(ctx);
  const launchesBefore = ctx.work.launches().length;
  session = await launchInstalled(ctx, { args: ["--autostart"] });
  assert.equal(await ctx.app.isMainVisible(), false);
  await ctx.launchSecondInstance({ binary: info.exe, args: [dirArg(ctx)] });
  await session.waitUntil(() => ctx.app.isMainVisible(), { message: "メインウィンドウが表示される" });

  const oldPid = ctx.work.pid();
  await startUpdate(ctx, session);
  const relaunched = await waitForUpdatedLaunch(ctx, { oldPid, launchesBefore: launchesBefore + 1 });

  // 同じ場所の実行ファイルが入れ替わり、同じ引数で起動し直して、自動起動の引数が付いていても、
  // 更新できたことが分かるようウィンドウを出している
  assert.equal(fileVersion(info.exe), NEW);
  assert.ok(sameFile(relaunched.exe, info.exe), `別の場所のアプリが起動しました: ${relaunched.exe}`);
  assert.ok(relaunched.args.includes(dirArg(ctx)), `更新前の引数が引き継がれていません: ${JSON.stringify(relaunched.args)}`);
  assert.ok(relaunched.args.includes("--autostart"), `更新前の引数が引き継がれていません: ${JSON.stringify(relaunched.args)}`);
  assert.equal(relaunched.mainShown, true, "ボタンを押して更新したあとに、ウィンドウを出していない");
  assert.equal(ctx.server.requestsTo("/update/files/").length, 1);
  assert.equal(installed()?.installLocation, info.installLocation);
  assert.ok(fs.existsSync(shortcuts().startMenu), "更新で、スタートメニューのショートカットが消えた");
  assert.equal(fs.existsSync(shortcuts().desktop), false, "利用者が消したデスクトップのショートカットを、更新で作り直した");

  // 起動し直したアプリを止めて、もう一度起動する: 更新前のデータが残っていて、新しい版になっている
  await ctx.stop();
  session = await launchInstalled(ctx);
  assert.ok((await noteTitles(session)).includes("更新の前に書いたメモ"), "更新の前に書いたノートがない");
  await openSettings(session);
  await session.waitForText(sel.settingsDialog, `Kiroscribe ${NEW}`);
  // 配布元の最新版と同じになったので、更新の知らせは出ない
  await closeSettings(session);
  assert.equal(await session.exists(sel.updateBanner), false, "更新したのに、まだ更新の知らせが出ている");
});

// 自動の更新は起動の直後に進むので、WebDriver でつながる前にアプリが終了しないよう、最初の確認を少し遅らせる
const AUTO_UPDATE_OPTIONS = { timeout: UPDATE_TIMEOUT, config: { timing: { updateFirstCheckMs: 10_000 } } };

test("[UPD-024][UPD-025][UPD-029] 自動インストールがオンなら、トレイにいるあいだにインストーラが新しい版を入れ、ウィンドウを出さずに起動し直す", AUTO_UPDATE_OPTIONS, async (ctx) => {
  // 古い版を入れ直す
  await uninstall();
  const info = await install(build.old.installer);
  assert.equal(info.displayVersion, OLD);

  // 自動インストールをオンにする（設定画面で）
  let session = await launchInstalled(ctx);
  await openSettings(session);
  const check = await session.$(settingsCheck("新しいバージョンを自動でインストールする"));
  if (!(await check.isSelected())) await check.click();
  await saveSettings(session);
  await closeSettings(session);
  await ctx.stop();

  // Windows の起動時と同じく、ウィンドウを出さずに（トレイに常駐して）起動する。何も操作しない
  publishNewVersion(ctx);
  const launchesBefore = ctx.work.launches().length;
  session = await launchInstalled(ctx, { args: ["--autostart"] });
  const oldPid = ctx.work.pid();
  assert.equal(await ctx.app.isMainVisible(), false);
  const relaunched = await waitForUpdatedLaunch(ctx, { oldPid, launchesBefore: launchesBefore + 1 });

  assert.equal(fileVersion(info.exe), NEW);
  assert.ok(relaunched.args.includes("--autostart"), JSON.stringify(relaunched.args));
  assert.equal(relaunched.mainShown, false, "自動の更新のあとに、ウィンドウを出した");

  // 次にウィンドウを開いたときに、更新したことを知らせる
  await ctx.stop();
  session = await launchInstalled(ctx);
  await session.waitForText(sel.updatedNotice, `バージョン ${NEW} に更新しました`);
});

test("[REL-009] 入っている版の上に新しいインストーラを実行すると、ノートと設定を残したまま新しい版になる", async (ctx) => {
  await uninstall();
  await install(build.old.installer);
  let session = await launchInstalled(ctx);
  await createNote(session, { title: "入れ直す前のメモ" });
  await ctx.stop();

  // 利用者がダウンロードした新しいインストーラを、そのまま実行したのと同じ
  const info = await install(build.new.installer);
  assert.equal(info.displayVersion, NEW);
  assert.equal(fileVersion(info.exe), NEW);
  session = await launchInstalled(ctx);
  assert.ok((await noteTitles(session)).includes("入れ直す前のメモ"), "入れ直す前に書いたノートがない");
  await openSettings(session);
  await session.waitForText(sel.settingsDialog, `Kiroscribe ${NEW}`);
});

test("[REL-011] アンインストールすると、アプリ・一覧の情報・ショートカットが消え、アプリのデータは残る", async () => {
  const info = installed() ?? (await install(build.old.installer));
  // 製品のデータの場所（%APPDATA%\<識別子>）に印を置き、アンインストールで消えないことを確かめる。
  // 本物のデータがある PC では、触らずに確かめを省く
  const dataDir = path.join(process.env.APPDATA, "dev.kiroscribe.desktop");
  const marker = path.join(dataDir, "kiroscribe-e2e-uninstall-check.txt");
  const checkData = !fs.existsSync(dataDir);
  if (checkData) {
    fs.mkdirSync(dataDir, { recursive: true });
    fs.writeFileSync(marker, "アンインストールで消えないこと");
  }
  try {
    await uninstall();
    assert.equal(installed(), null);
    assert.equal(fs.existsSync(info.exe), false, `実行ファイルが残っています: ${info.exe}`);
    assert.equal(fs.existsSync(shortcuts().startMenu), false, "スタートメニューのショートカットが残っています");
    assert.equal(fs.existsSync(shortcuts().desktop), false, "デスクトップのショートカットが残っています");
    if (checkData) assert.ok(fs.existsSync(marker), "アンインストールで、アプリのデータが消えた");
  } finally {
    if (checkData) fs.rmSync(dataDir, { recursive: true, force: true });
  }
});
