// 会議の自動検出: 会議が始まるとノートを作って録音し、終わると自動で止める。
// 会議中かどうかは、作業フォルダの meeting.txt でテストが決める（ctx.work.setMeeting）。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";
import { closeSettings, noteTitles, openSettings, saveSettings, sel, settingsCheck } from "../lib/ui.mjs";
import { byText } from "../lib/webdriver.mjs";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
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

test("[MTG-012] 設定の「会議の自動検出」がオフのときは、会議を検出しても何もしない", async (ctx) => {
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
});
