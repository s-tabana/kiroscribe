// 常駐: ウィンドウを閉じてもアプリは動き続け、もう一度起動すると既存のウィンドウが出る。
// タスクトレイのメニューは WebDriver から操作できないので、プロセスとウィンドウの状態で確かめる。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";
import { createNote, noteTitles } from "../lib/ui.mjs";

test("[TRY-001][TRY-006][IPC-007] メインウィンドウを閉じてもプロセスは残り、2 回目の起動では既存のウィンドウが表示される（多重起動しない）", async (ctx) => {
  const session = await ctx.launch();
  await createNote(session, { title: "常駐のテスト" });
  const pid = ctx.work.pid();
  assert.ok(pid > 0);
  assert.equal(await ctx.app.isMainVisible(), true);

  // ウィンドウを閉じる（「×」を押したのと同じ）→ 見えなくなるが、プロセスは残る
  ctx.work.closeWindow("main");
  await session.waitUntil(async () => !(await ctx.app.isMainVisible()), { message: "メインウィンドウが見えなくなる" });
  assert.equal(ctx.app.isRunning(), true, "ウィンドウを閉じたらプロセスが終了した");

  // 閉じているあいだも、会議の検出と録音は続く
  ctx.work.setMeeting("teams");
  await session.waitUntil(() => ctx.server.transcriptions.length > 0, {
    timeout: 30_000,
    message: "ウィンドウを閉じていても会議が録音される",
  });
  assert.equal(await ctx.app.isMainVisible(), false, "会議を検出してもメインウィンドウは出さない");

  // もう一度起動する → 2 つ目はすぐ終了し、1 つ目のウィンドウが出る
  const second = await ctx.launchSecondInstance();
  assert.equal(second.code, 0, `2 つ目の終了コード（シグナル: ${second.signal}）`);
  await session.waitUntil(() => ctx.app.isMainVisible(), { message: "既存のメインウィンドウが表示される" });
  assert.equal(ctx.work.pid(), pid, "動いているのは 1 つ目のプロセスのまま");
  assert.equal(ctx.app.isRunning(), true);

  // 1 つ目の状態はそのまま（録音も続いている）
  await ctx.app.main();
  const titles = await noteTitles(session);
  assert.equal(titles.length, 2);
  assert.match(titles[0], /^Teams /);
  assert.equal(titles[1], "常駐のテスト");
  await session.$('nav[aria-label="ノート一覧"] [aria-label="録音中"]');
});

test("[TRY-008] Windows の起動時の自動起動（--autostart 付き）では、ウィンドウを出さずに常駐して会議を待つ", async (ctx) => {
  const session = await ctx.launch({ args: ["--autostart"] });
  assert.equal(await ctx.app.isMainVisible(), false, "自動起動なのにメインウィンドウを出している");
  assert.equal(ctx.app.isRunning(), true);

  // ウィンドウを出していなくても、会議を検出して録音する
  ctx.work.setMeeting("googleMeet");
  await session.waitUntil(() => ctx.server.transcriptions.length > 0, {
    timeout: 30_000,
    message: "常駐しているあいだに会議が録音される",
  });
  assert.equal(await ctx.app.isMainVisible(), false);

  // もう一度（ふつうに）起動すると、常駐していたほうのウィンドウが出る
  const second = await ctx.launchSecondInstance();
  assert.equal(second.code, 0);
  await session.waitUntil(() => ctx.app.isMainVisible(), { message: "メインウィンドウが表示される" });
  await ctx.app.main();
  await session.waitUntil(async () => /^Google Meet /.test((await noteTitles(session))[0] ?? ""), {
    message: "会議のノートが一覧にある",
  });
});

test(
  "[TRY-010] タスクトレイのアイコンを作れない環境でも、アプリは起動して使える",
  { config: { trayUnavailable: true } },
  async (ctx) => {
    const session = await ctx.launch();
    assert.equal(await ctx.app.isMainVisible(), true);
    assert.match(ctx.work.logs(), /タスクトレイを作成できませんでした/, "トレイを作れなかったことが、このテストの前提");
    await createNote(session, { title: "トレイなしでも使える", memo: "メモ" });
    assert.deepEqual(await noteTitles(session), ["トレイなしでも使える"]);
    // 会議の検出も動く
    ctx.work.setMeeting("zoom");
    await ctx.app.popup();
  },
);
