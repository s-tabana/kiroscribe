// 会議の自動検出: 会議が始まるとノートを作って録音し、終わると自動で止める。
// 会議中かどうかは、作業フォルダの meeting.txt でテストが決める（ctx.work.setMeeting）。

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { ROOT, test } from "../lib/app.mjs";
import { barButton, closeSettings, noteTitles, openSettings, pressBarButton, saveSettings, sel, settingsCheck } from "../lib/ui.mjs";
import { ANTHROPIC_KEY, OPENAI_KEY } from "../lib/mock-server.mjs";
import { dragWindow } from "../lib/uia.mjs";
import { byText } from "../lib/webdriver.mjs";
import { readZip } from "../lib/zip.mjs";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * 利用者の名前（ログオンの名前と、利用者のフォルダの名前）が入っていないことを確かめる。
 * 大文字と小文字の違い、JSON の中で区切りが \\ と書かれたパスも見る
 */
function assertNoUserFolderName(text, ...userNames) {
  const lower = text.toLowerCase();
  for (const name of new Set(userNames.filter(Boolean).map((n) => n.toLowerCase()))) {
    for (const root of ["users", "home"]) {
      for (const sep of ["\\", "\\\\", "/"]) {
        const needle = `${sep}${root}${sep}${name}`;
        assert.ok(!lower.includes(needle), `利用者のフォルダの名前が zip に入っている: ${needle}`);
      }
    }
    // 短い名前と、伏せた印（<user>）と同じ名前は、ほかの語の一部にも現れるので、パスの形でだけ確かめる
    if (name.length >= 4 && name !== "user") assert.ok(!lower.includes(name), `利用者の名前が zip に入っている: ${name}`);
  }
}

/** 会議が見えなくなってから録音を止めるまでの猶予（このテストでの値。製品では 20 秒） */
const GRACE_MS = 3_000;

test(
  "[MTG-011][MTG-016][MTG-022][MIN-018][POP-001][POP-002][POP-004][POP-008][REC-016][IPC-005][IPC-007] 会議が始まるとノートが作られて録音が始まり、終わると猶予のあと自動で止まり、ポップアップから議事録を作れる",
  { config: { timing: { meetingEndGraceMs: GRACE_MS } } },
  async (ctx) => {
    const session = await ctx.launch();
    assert.equal(await ctx.app.hasPopup(), false);

    // 会議が始まる → ポップアップが出て、録音が始まる
    ctx.work.setMeeting("googleMeet");
    await ctx.app.popup();
    const started = await session.waitForText(sel.popup, "自動で録音・文字起こしを始めました");
    assert.ok(started.includes("Google Meet"), started);
    // ポップアップは、主モニターの右下（右端から 16px、下端から 64px 内側）に、幅 380px・高さ 128px で出る
    const place = await session.execute(
      "return { width: window.innerWidth, height: window.innerHeight, screenWidth: screen.width, screenHeight: screen.height }",
    );
    assert.equal(place.width, 380, "ポップアップの幅");
    // 高さは 128px。Linux（開発用）では、ウィンドウの高さに下限があり、それより低くはならない
    if (process.platform === "win32") assert.equal(place.height, 128, "ポップアップの高さ");
    else assert.ok(place.height >= 128, `ポップアップの高さ: ${place.height}`);
    const rect = await session.windowRect();
    assert.deepEqual(
      [rect.x, rect.y],
      [place.screenWidth - 380 - 16, place.screenHeight - 128 - 64],
      `ポップアップの位置（画面 ${place.screenWidth}x${place.screenHeight}）`,
    );
    // 録音の状態は、ポップアップにも出る（経過時間）
    await session.waitForText(`${sel.popup} [role="timer"]`, /\d:\d\d/);

    // メイン画面: 会議のノートが一覧に加わり、録音中の印が付く
    await ctx.app.main();
    await session.waitUntil(async () => (await noteTitles(session)).length === 1, { message: "会議のノートができる" });
    const [title] = await noteTitles(session);
    assert.match(title, /^Google Meet \d{1,2}\/\d{1,2} \d{2}:\d{2}$/);
    await session.$(`${sel.noteItems} [aria-label="録音中"]`);
    await session.waitUntil(() => ctx.server.transcriptions.length > 0, {
      timeout: 30_000,
      message: "会議の音声が文字起こしされる",
    });

    // ポップアップを × で閉じても、録音は続く
    await ctx.app.popup();
    await session.clickClosing(`${sel.popup} button[aria-label="閉じる"]`);
    await ctx.app.main();
    await session.waitUntil(async () => !(await ctx.app.hasPopup()), { message: "ポップアップが閉じる" });
    await session.$(`${sel.noteItems} [aria-label="録音中"]`);

    // 会議が終わる → 猶予のあいだは録音を続ける（一時的に会議が見えなくなっただけかもしれないので）
    ctx.server.openai.delayMs = 3_000; // 仕上げ（残りの文字起こし）のあいだの表示を確かめられるようにする
    ctx.work.setMeeting(null);
    const endedAt = Date.now();
    await sleep(GRACE_MS / 2);
    await session.$(`${sel.noteItems} [aria-label="録音中"]`);
    assert.equal(await ctx.app.hasPopup(), false, "猶予のあいだに止まった");

    // 猶予がたつと自動で止まり、ポップアップが知らせる（閉じていたポップアップは、そのときの内容で出し直す）
    await ctx.app.popup({ timeout: 30_000 });
    assert.ok(Date.now() - endedAt >= GRACE_MS - 300, `猶予（${GRACE_MS} ミリ秒）より早く止まった`);
    await session.waitForText(sel.popup, "会議が終わりました。文字起こしを仕上げています…");
    const stopped = await session.waitForText(sel.popup, "録音を停止しました", { timeout: 30_000 });
    assert.ok(stopped.includes(`「${title}」に保存しています`), stopped);
    ctx.server.openai.delayMs = 0;
    // 知らせが入れ替わっても、ポップアップのウィンドウは 1 つだけ
    assert.equal((await ctx.app.popupHandles()).length, 1);
    // メイン画面も、録音していない表示に変わる
    assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false);

    // ポップアップの［議事録を作成］ → メイン画面でそのノートが開き、議事録ができる
    await ctx.app.popup();
    await session.clickClosing(byText("button", "議事録を作成"));
    await ctx.app.main();
    await session.waitForText(`${sel.minutes}[aria-busy="false"]`, "リリースは 12/5", { timeout: 30_000 });
    assert.equal(await (await session.$(sel.title)).value(), title);
    assert.equal(ctx.server.messages.length, 1);
    assert.ok(
      ctx.server.messages[0].json.messages[0].content.includes("模擬の文字起こし"),
      "議事録の材料に、会議の文字起こしが含まれる",
    );
    await session.waitUntil(async () => !(await ctx.app.hasPopup()), { message: "ポップアップが閉じる" });
  },
);

test("[POP-002][POP-004][POP-007][REC-016] 会議の録音は、ポップアップの［停止］で止められ、会議が続いていても録音し直さない", async (ctx) => {
  const session = await ctx.launch();
  // メイン画面で検索しているあいだに会議が始まる
  await (await session.$(sel.search)).type("ほかの言葉");
  ctx.work.setMeeting("zoom");
  await ctx.app.popup();
  await session.waitForText(sel.popup, "自動で録音・文字起こしを始めました");
  const [popupHandle] = await ctx.app.popupHandles();
  await ctx.app.popup();
  await session.waitUntil(() => ctx.server.transcriptions.length > 0, { timeout: 30_000, message: "会議の音声が文字起こしされる" });

  // ［停止］: 仕上げのあいだの文は、会議が終わって止まるときとは別。終わると、保存先のノートを知らせる
  ctx.server.openai.delayMs = 2_000;
  await session.click(byText("button", "停止"));
  await session.waitForText(sel.popup, "録音を停止しました。文字起こしを仕上げています…");
  const stopped = await session.waitForText(sel.popup, /録音を停止しました。「/, { timeout: 30_000 });
  assert.match(stopped, /「Zoom \d{1,2}\/\d{1,2} \d{2}:\d{2}」に保存しています/);
  ctx.server.openai.delayMs = 0;
  // 表示は同じウィンドウの中で入れ替わる（新しいウィンドウは出さない）
  assert.deepEqual(await ctx.app.popupHandles(), [popupHandle]);

  // ［ノートを開く］でメイン画面にそのノートが出る（検索は空に戻る）。会議はまだ続いているが、新しいノートは作られない
  await ctx.app.popup();
  await session.clickClosing(byText("button", "ノートを開く"));
  await ctx.app.main();
  await session.waitUntil(async () => /^Zoom /.test(await (await session.$(sel.title)).value()), {
    message: "会議のノートが開く",
  });
  assert.equal(await (await session.$(sel.search)).value(), "");
  assert.equal(await ctx.app.isMainVisible(), true);
  await session.waitUntil(async () => !(await ctx.app.hasPopup()), { message: "ポップアップが閉じる" });
  await sleep(1_500);
  assert.equal((await noteTitles(session)).length, 1);
  assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false);
  assert.equal(await ctx.app.hasPopup(), false, "止めたあとに、同じ会議の録音を始め直している");
});

test(
  "[POP-009] API キーが未設定のときに会議が始まったら、録音せずに設定へ案内する",
  { apiKeys: false },
  async (ctx) => {
    const session = await ctx.launch();
    ctx.work.setMeeting("slackHuddle");
    await ctx.app.popup();
    const shown = await session.waitForText(
      sel.popup,
      "OpenAI の API キーが設定されていないため、自動で録音できませんでした",
    );
    assert.ok(shown.includes("Slack ハドル"), shown);

    await session.clickClosing(byText("button", "設定を開く"));
    await ctx.app.main();
    await session.$(sel.settingsDialog);
    assert.deepEqual(await noteTitles(session), [], "録音できないときはノートを作らない");
    assert.equal(ctx.server.transcriptions.length, 0);
    await session.waitUntil(async () => !(await ctx.app.hasPopup()), { message: "ポップアップが閉じる" });
  },
);

test("[MTG-012][MTG-019] 設定の「会議の自動検出」がオフのときは、会議を検出しても何もしない。会議の途中でオンにすると、録音を始める", async (ctx) => {
  const session = await ctx.launch();
  await openSettings(session);
  await session.click(settingsCheck("会議を検出したら自動で録音・文字起こしする"));
  await saveSettings(session);
  await closeSettings(session);

  ctx.work.setMeeting("discord");
  await sleep(2_000); // 開始の確定（このテストでは 0.3 秒）より十分に長く待つ
  assert.equal(await ctx.app.hasPopup(), false, "ポップアップを出している");
  assert.deepEqual(await noteTitles(session), [], "ノートを作っている");
  assert.equal(ctx.server.transcriptions.length, 0, "録音している");

  // 会議が続いているあいだにオンにすると、その会議の録音を始める
  await openSettings(session);
  await session.click(settingsCheck("会議を検出したら自動で録音・文字起こしする"));
  await saveSettings(session);
  await closeSettings(session);
  await ctx.app.popup();
  const started = await session.waitForText(sel.popup, "自動で録音・文字起こしを始めました");
  assert.ok(started.includes("Discord"), started);
  await ctx.app.main();
  await session.waitUntil(async () => /^Discord /.test((await noteTitles(session))[0] ?? ""), { message: "会議のノートができる" });
  await session.$(`${sel.noteItems} [aria-label="録音中"]`);
});

test("[MTG-010] 会議が終わってすぐ別のアプリの会議が始まったら、前の録音を止めて、新しいノートで録音を始める", async (ctx) => {
  const session = await ctx.launch();
  ctx.work.setMeeting("googleMeet");
  await ctx.app.popup();
  await session.waitForText(sel.popup, "自動で録音・文字起こしを始めました");
  await ctx.app.main();
  await session.waitUntil(() => ctx.server.transcriptions.length > 0, { timeout: 30_000, message: "最初の会議が文字起こしされる" });

  // Google Meet を抜けて、すぐ Zoom の会議に入る（前の会議の終了を待つ 2 秒より前に、次の会議が始まる）
  ctx.server.openai.delayMs = 1_500; // 前の録音の仕上げに時間がかかっても、次の会議を取りこぼさない
  ctx.work.setMeeting("zoom");
  await session.waitUntil(async () => (await noteTitles(session)).length === 2, { timeout: 30_000, message: "次の会議のノートができる" });
  ctx.server.openai.delayMs = 0;
  const titles = await noteTitles(session);
  assert.match(titles[0], /^Zoom /);
  assert.match(titles[1], /^Google Meet /);
  // 録音しているのは、新しいノートだけ
  await session.waitUntil(
    () =>
      session.execute(`
        const items = Array.from(document.querySelectorAll(${JSON.stringify(sel.noteItems)}));
        return items.length === 2 && !!items[0].querySelector('[aria-label="録音中"]') && !items[1].querySelector('[aria-label="録音中"]');
      `),
    { timeout: 30_000, message: "新しい会議のノートだけが録音中になる" },
  );
  await ctx.app.popup();
  const popup = await session.waitForText(sel.popup, "自動で録音・文字起こしを始めました", { timeout: 30_000 });
  assert.ok(popup.includes("Zoom"), popup);
  assert.equal((await ctx.app.popupHandles()).length, 1);
});

test("[POP-011] 自動で始めた録音をメインウィンドウの［停止］で止めると、ポップアップも停止の表示に変わる", async (ctx) => {
  const session = await ctx.launch();
  ctx.work.setMeeting("teams");
  await ctx.app.popup();
  await session.waitForText(sel.popup, "自動で録音・文字起こしを始めました");

  // メインウィンドウで会議のノートを開いて、［停止］を押す
  await ctx.app.main();
  await session.waitUntil(async () => (await noteTitles(session)).length === 1, { message: "会議のノートができる" });
  const [title] = await noteTitles(session);
  await session.click(`${sel.noteItems}`);
  await session.waitForText(`${sel.recordingBar} [role="timer"]`, "録音中");
  await session.waitUntil(() => ctx.server.transcriptions.length > 0, { timeout: 30_000, message: "会議の音声が文字起こしされる" });
  await pressBarButton(session, "停止");
  await session.$(barButton("録音を再開"), { timeout: 30_000 });

  // ポップアップは、録音中の表示のまま残らない
  await ctx.app.popup();
  const stopped = await session.waitForText(sel.popup, "録音を停止しました。「");
  assert.ok(stopped.includes(`「${title}」に保存しています`), stopped);
  assert.equal(await session.exists(`${sel.popup} button.popup-stop`), false, "ポップアップに［停止］が残っている");
  // 会議は続いているが、録音は始め直さない
  await ctx.app.main();
  await sleep(1_500);
  assert.equal((await noteTitles(session)).length, 1);
  assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false);
});

test("[MTG-015] 自動録音を始められなかったときは、理由を知らせて、空のノートを残さない", async (ctx) => {
  const reason = "マイクが見つかりません。接続と Windows のプライバシー設定（マイクへのアクセス）を確認してください";
  ctx.work.setMicError(reason);
  const session = await ctx.launch();
  ctx.work.setMeeting("zoom");
  await ctx.app.popup();
  const shown = await session.waitForText(sel.popup, reason);
  assert.ok(shown.includes("Zoom"), shown);

  await ctx.app.main();
  await sleep(500);
  assert.deepEqual(await noteTitles(session), [], "録音できなかった会議のノートが、一覧に残っている");
  assert.equal(ctx.server.transcriptions.length, 0);
  // 起動し直しても、空のノートは出てこない
  ctx.work.setMeeting(null);
  const relaunched = await ctx.relaunch();
  await relaunched.waitForText('nav[aria-label="ノート一覧"]', "まだノートがありません");
});

test(
  "[MTG-025][MTG-026][LOG-011] 設定の［デバッグ用のログを保存…］で、ログ・会議の検出の記録・環境の情報を zip で保存できる",
  { config: { realMeetingDetection: true } },
  async (ctx) => {
    const session = await ctx.launch();
    // 検出を何回か調べるのを待つ（製品と同じく、調べた様子を記録している）
    await new Promise((resolve) => setTimeout(resolve, 1_500));
    const out = `${ctx.work.dir}${process.platform === "win32" ? "\\" : "/"}debug.zip`;
    ctx.work.answerDialogs({ savePath: out });
    await openSettings(session);
    await session.click(byText("button", "デバッグ用のログを保存…"));
    const shown = await session.waitForText(sel.settingsDialog, /デバッグ用のログを保存しました: /);
    assert.ok(shown.includes(out), shown);
    const saveDialog = ctx.work.dialogs().find((d) => d.type === "save");
    assert.match(saveDialog.options.defaultPath, /^kiroscribe-debug-\d{8}-\d{4}\.zip$/);

    const files = readZip(fs.readFileSync(out));
    const names = [...files.keys()];
    for (const name of ["README.txt", "environment.json", "meeting-detection.json"]) {
      assert.ok(names.includes(name), `${name} がない: ${names.join(", ")}`);
    }
    const logNames = names.filter((name) => /^logs\/kiroscribe-\d{8}\.log$/.test(name));
    assert.ok(logNames.length >= 1, names.join(", "));

    // 会議の検出の記録は、回帰テストの材料と同じ形で入る
    const record = JSON.parse(files.get("meeting-detection.json").toString("utf8"));
    assert.equal(record.format, "kiroscribe-meeting-record/4");
    assert.ok(record.entries.length >= 1, JSON.stringify(record));
    assert.equal(typeof record.rulesVersion, "number");
    // フルパス（利用者の名前を含む）は残さない。詳しい記録がオフなので、窓の題も残さない
    for (const entry of record.entries) {
      assert.equal(entry.titles, undefined, JSON.stringify(entry));
      for (const service of entry.services) {
        for (const context of service.contexts) {
          assert.ok(!/[\\/]/.test(context.app), `アプリのフルパスが記録に入っている: ${context.app}`);
        }
      }
    }

    // ログには、本体の起動・会議の検出の判断と、画面の出来事が入る
    const log = logNames.map((name) => files.get(name).toString("utf8")).join("");
    for (const event of ["app.started", "meeting.check", "ui.loaded", "log.bundle.saving"]) {
      assert.ok(log.includes(`"event":"${event}"`), `${event} がログにない:\n${log}`);
    }
    const environment = JSON.parse(files.get("environment.json").toString("utf8"));
    assert.equal(environment.appVersion, JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).version);

    const all = names.map((name) => files.get(name).toString("utf8")).join("\n");
    assertNoUserFolderName(all, os.userInfo().username, path.basename(os.homedir()));
    for (const key of [OPENAI_KEY, ANTHROPIC_KEY]) assert.ok(!all.includes(key), "API キーが zip に入っている");
    await closeSettings(session);
  },
);

test(
  "[POP-012] ポップアップは、上部を本物のマウスでドラッグして動かせる",
  { skip: process.platform !== "win32" && "Windows の本物のマウスの操作で確かめるテスト" },
  async (ctx) => {
    await ctx.launch();
    ctx.work.setMeeting("zoom");
    await ctx.app.popup();
    const moved = dragWindow(ctx.work.pid(), { maxWidth: 500, grabX: 60, grabY: 14, dx: -240, dy: -160 });
    assert.ok(moved, "ポップアップのウィンドウが見つかりません");
    const [left, top] = moved.before;
    const [newLeft, newTop] = moved.after;
    // ドラッグした分だけ動く（枠の端数を見込んで、数ピクセルの差は許す）
    assert.ok(Math.abs(newLeft - (left - 240)) <= 4 && Math.abs(newTop - (top - 160)) <= 4, JSON.stringify(moved));
    ctx.work.setMeeting(null);
  },
);
