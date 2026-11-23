// 会議の自動検出: 会議が始まるとノートを作って録音し、終わると自動で止める。
// 会議中かどうかは、作業フォルダの meeting.txt でテストが決める（ctx.work.setMeeting）。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";
import { noteTitles, sel } from "../lib/ui.mjs";
import { byText } from "../lib/webdriver.mjs";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
/** 会議が見えなくなってから録音を止めるまでの猶予（このテストでの値。製品では 20 秒） */
const GRACE_MS = 3_000;

test(
  "会議が始まるとノートが作られて録音が始まり、終わると猶予のあと自動で止まり、ポップアップから議事録を作れる",
  { config: { timing: { meetingEndGraceMs: GRACE_MS } } },
  async (ctx) => {
    const session = await ctx.launch();
    assert.equal(await ctx.app.hasPopup(), false);

    // 会議が始まる → ポップアップが出て、録音が始まる
    ctx.work.setMeeting("googleMeet");
    await ctx.app.popup();
    const started = await session.waitForText(sel.popup, "自動で録音・文字起こしを始めました");
    assert.ok(started.includes("Google Meet"), started);

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

    // 会議が終わる → 猶予のあいだは録音を続ける（一時的に会議が見えなくなっただけかもしれないので）
    ctx.work.setMeeting(null);
    const endedAt = Date.now();
    await sleep(GRACE_MS / 2);
    await session.$(`${sel.noteItems} [aria-label="録音中"]`);
    await ctx.app.popup();
    assert.ok((await session.text(sel.popup)).includes("自動で録音・文字起こしを始めました"), "猶予のあいだに止まった");

    // 猶予がたつと自動で止まり、ポップアップが知らせる
    const stopped = await session.waitForText(sel.popup, "録音を停止しました", { timeout: 30_000 });
    assert.ok(Date.now() - endedAt >= GRACE_MS - 300, `猶予（${GRACE_MS} ミリ秒）より早く止まった`);
    assert.ok(stopped.includes(`「${title}」に保存しています`), stopped);

    // ポップアップの［議事録を作成］ → メイン画面でそのノートが開き、議事録ができる
    await session.clickClosing(byText("button", "議事録を作成"));
    await ctx.app.main();
    await session.waitForText(`${sel.minutes}[aria-busy="false"]`, "リリースは 12/5", { timeout: 30_000 });
    assert.equal(await (await session.$(sel.title)).value(), title);
    assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false);
    assert.equal(ctx.server.messages.length, 1);
    assert.ok(
      ctx.server.messages[0].json.messages[0].content.includes("模擬の文字起こし"),
      "議事録の材料に、会議の文字起こしが含まれる",
    );
    await session.waitUntil(async () => !(await ctx.app.hasPopup()), { message: "ポップアップが閉じる" });
  },
);

test("会議の録音は、ポップアップの［停止］で止められ、会議が続いていても録音し直さない", async (ctx) => {
  const session = await ctx.launch();
  ctx.work.setMeeting("zoom");
  await ctx.app.popup();
  await session.waitForText(sel.popup, "自動で録音・文字起こしを始めました");
  await session.click(byText("button", "停止"));
  const stopped = await session.waitForText(sel.popup, "録音を停止しました", { timeout: 30_000 });
  assert.match(stopped, /「Zoom \d{1,2}\/\d{1,2} \d{2}:\d{2}」に保存しています/);

  // ［ノートを開く］でメイン画面にそのノートが出る。会議はまだ続いているが、新しいノートは作られない
  await session.clickClosing(byText("button", "ノートを開く"));
  await ctx.app.main();
  await session.waitUntil(async () => /^Zoom /.test(await (await session.$(sel.title)).value()), {
    message: "会議のノートが開く",
  });
  await sleep(1_500);
  assert.equal((await noteTitles(session)).length, 1);
  assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false);
});

test(
  "API キーが未設定のときに会議が始まったら、録音せずに設定へ案内する",
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
