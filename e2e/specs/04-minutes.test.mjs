// 議事録の作成: ストリーミングでの表示と保存、途中での中止、API のエラーの知らせ方。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";
import { ANTHROPIC_KEY, createGate } from "../lib/mock-server.mjs";
import { createNote, dismissToast, noteTitles, openNote, pressBarButton, sel, toastText } from "../lib/ui.mjs";

const streaming = `${sel.minutes}[aria-busy="true"]`;
const finished = `${sel.minutes}[aria-busy="false"]`;
const enhanceButton = 'xpath://*[@aria-label="録音"]//button[normalize-space(.)="議事録を作成"]';

test("[MIN-004][MIN-006][NOTE-019] ［議事録を作成］で、議事録がストリーミングで表示され、保存される", async (ctx) => {
  // 文字が届く前と、前半を送ったところで止めておき、途中の表示を確かめてから残りを送る
  const before = createGate();
  const gate = createGate();
  ctx.server.anthropic.reply = () => ({
    before: before.promise,
    chunks: ["# 模擬の議事録\n\n", "## 決定事項\n", "- リリースは 12/5\n"],
    hold: gate.promise,
    tail: ["\n## ToDo\n", "- 田中: 資料を送る\n"],
  });
  let session = await ctx.launch();
  // タイトルは空のまま（議事録の見出しがタイトルになる）
  await createNote(session, { memo: "- リリース日を決める" });

  await pressBarButton(session, "議事録を作成");
  // 文字が届く前
  await session.waitForText(streaming, "メモと文字起こしを読んでいます…");
  before.open();
  // 生成の途中: 届いたところまでが表示され、まだ後半はない。先頭のタイトル行は表示しない
  const partial = await session.waitForText(streaming, "リリースは 12/5");
  assert.ok(partial.includes("議事録を作成しています…"), partial);
  assert.ok(!partial.includes("メモと文字起こしを読んでいます…"), partial);
  assert.ok(!partial.includes("模擬の議事録"), "作成中の表示に、タイトル行が出ている");
  assert.ok(!partial.includes("資料を送る"), "途中の表示に、まだ届いていない部分が出ている");
  await session.$(`xpath://section[@aria-label="議事録"]//button[normalize-space(.)="中止"]`);
  gate.open();

  // 完了: 全文が表示され、タイトルが付く
  const minutes = await session.waitForText(finished, "田中: 資料を送る");
  assert.match(minutes, /決定事項[\s\S]*リリースは 12\/5[\s\S]*ToDo/);
  assert.ok(!minutes.includes("模擬の議事録"), "見出し（タイトル）は本文に含めない");
  await session.waitUntil(async () => (await (await session.$(sel.title)).value()) === "模擬の議事録", {
    message: "議事録の見出しがノートのタイトルになる",
  });
  assert.deepEqual(await noteTitles(session), ["模擬の議事録"]);

  // API に送った内容
  assert.equal(ctx.server.messages.length, 1);
  const { headers, json } = ctx.server.messages[0];
  assert.equal(headers["x-api-key"], ANTHROPIC_KEY);
  assert.equal(json.model, "claude-opus-5-5");
  assert.equal(json.stream, true);
  assert.equal(json.messages.length, 1);
  assert.equal(json.messages[0].role, "user");
  assert.ok(json.messages[0].content.includes("- リリース日を決める"), "メモが含まれる");
  assert.equal(json.system[0].type, "text");

  // 「自分のメモ」に切り替えると元のメモが残っている
  await session.click('xpath://div[@role="tablist"][@aria-label="表示の切り替え"]/button[normalize-space(.)="自分のメモ"]');
  assert.equal(await (await session.$(sel.memo)).value(), "- リリース日を決める");

  // 再起動しても議事録が残っている（開くと議事録が先に表示される）
  session = await ctx.relaunch();
  await session.waitForText(finished, "田中: 資料を送る");
  assert.equal(ctx.server.messages.length, 1, "保存済みの議事録を表示するだけで、API は呼ばない");
});

test("[MIN-005] 議事録の作成は途中で中止でき、中止したものは保存されない", async (ctx) => {
  const gate = createGate();
  ctx.server.anthropic.reply = () => ({ chunks: ["# 途中のタイトル\n\n", "書きかけの議事録"], hold: gate.promise });
  try {
    const session = await ctx.launch();
    await createNote(session, { title: "中止のテスト", memo: "- メモ" });
    await pressBarButton(session, "議事録を作成");
    await session.waitForText(streaming, "書きかけの議事録");

    await session.click(`xpath://section[@aria-label="議事録"]//button[normalize-space(.)="中止"]`);
    // メモの画面に戻り、もう一度作成できる。中止は失敗として知らせない
    await session.$(enhanceButton);
    await session.waitGone(sel.minutes);
    assert.equal(await session.exists(sel.toast), false, "中止はエラーとして知らせない");
    assert.equal(await (await session.$(sel.memo)).value(), "- メモ");
    assert.equal(await (await session.$(sel.title)).value(), "中止のテスト");

    // 保存されていない（再読み込みしても議事録はない）
    await ctx.relaunch().then(async (relaunched) => {
      await relaunched.$(sel.memo);
      assert.equal(await relaunched.exists(sel.minutes), false);
    });
  } finally {
    gate.open();
  }
});

/** API の失敗 1 種類ぶん: 応答の決め方と、画面に出るはずの知らせ */
const failures = [
  {
    name: "キーの誤り（401）",
    reply: () => ({ status: 401 }),
    message: "Anthropic の API キーが正しくないか、権限がありません（HTTP 401: invalid x-api-key）。設定画面でキーを確認してください",
    requests: 1,
    opensSettings: true,
  },
  {
    name: "利用上限（429）",
    reply: () => ({ status: 429 }),
    message: "Anthropic の利用上限に達しました。しばらく待ってから試してください（HTTP 429:",
    requests: 4, // 最初の 1 回 + 再試行 3 回
  },
  {
    name: "サーバーのエラー（500）",
    reply: () => ({ status: 500 }),
    message: "Anthropic がエラーを返しました（HTTP 500）: Internal server error",
    requests: 4,
  },
  {
    name: "回答を控えた（refusal）",
    reply: () => ({ chunks: [], stopReason: "refusal" }),
    message: "Claude がこの内容への回答を控えました。メモや文字起こしの内容を確認して、もう一度お試しください",
    requests: 1,
  },
  {
    name: "途中で切れた（ストリームの途中のエラー）",
    reply: () => ({ chunks: ["# タイトル\n\n途中まで"], errorAfter: { type: "overloaded_error", message: "Overloaded" } }),
    message: "Anthropic がエラーを返しました（HTTP 529）: Overloaded",
    requests: 1, // 文字が出始めたあとは再試行しない
  },
];

test("[MIN-017][ERR-003] 議事録の API がエラーを返したら、原因の分かるメッセージが出て、メモはそのまま残る", async (ctx) => {
  const session = await ctx.launch();
  await createNote(session, { title: "エラーのテスト", memo: "- メモ" });

  for (const failure of failures) {
    const before = ctx.server.messages.length;
    ctx.server.anthropic.reply = failure.reply;
    await pressBarButton(session, "議事録を作成");
    const shown = await toastText(session, failure.message);
    assert.ok(shown.includes(failure.message), `${failure.name}: ${shown}`);
    assert.equal(ctx.server.messages.length - before, failure.requests, `${failure.name}: API を呼んだ回数`);
    // 失敗したら議事録は作られず、メモの画面に戻る
    await session.$(enhanceButton);
    assert.equal(await session.exists(sel.minutes), false, failure.name);
    assert.equal(await (await session.$(sel.memo)).value(), "- メモ");
    assert.equal(await session.exists(`${sel.toast} button.button`), Boolean(failure.opensSettings), `${failure.name}: ［設定を開く］の有無`);
    await dismissToast(session);
  }

  // エラーのあとでも、直れば作成できる
  ctx.server.anthropic.reply = () => ({ chunks: ["# タイトル\n\n", "直ったあとの議事録"] });
  await pressBarButton(session, "議事録を作成");
  await session.waitForText(finished, "直ったあとの議事録");
});

test("[MIN-001] メモも文字起こしもないノートでは、議事録を作成するボタンが出ない", async (ctx) => {
  const session = await ctx.launch();
  await createNote(session, { title: "空のノート" });
  await session.$('xpath://*[@aria-label="録音"]//button[normalize-space(.)="録音を開始"]');
  assert.equal(await session.exists(enhanceButton), false);
  assert.equal(ctx.server.messages.length, 0);
});

test("[MIN-019] 議事録の作成中に別のノートへ移っても作成は続き、戻ると途中の文章とともに作成中の表示になる", async (ctx) => {
  const first = createGate();
  const second = createGate();
  let calls = 0;
  ctx.server.anthropic.reply = () => {
    calls += 1;
    const gate = calls === 1 ? first : second;
    return { chunks: [`# 議事録 ${calls}\n\n`, "## 決定事項\n", `- ${calls} つ目の前半\n`], hold: gate.promise, tail: [`- ${calls} つ目の後半\n`] };
  };
  try {
    const session = await ctx.launch();
    await createNote(session, { title: "ほかのノート", memo: "- 別のメモ" });
    await createNote(session, { title: "作成中に戻る", memo: "- メモ A" });
    await createNote(session, { title: "終わってから戻る", memo: "- メモ B" });

    // 1 つ目: 作成中に別のノートへ移り、作成中のうちに戻る
    await openNote(session, "作成中に戻る");
    await pressBarButton(session, "議事録を作成");
    await session.waitForText(streaming, "1 つ目の前半");
    await openNote(session, "ほかのノート");
    assert.equal(await session.exists(sel.minutes), false, "別のノートに、作成中の議事録が出ている");
    assert.equal(await (await session.$(sel.memo)).value(), "- 別のメモ");
    await openNote(session, "作成中に戻る");
    // 戻ると、それまでに届いた文章とともに、作成中の表示になる
    const resumed = await session.waitForText(streaming, "1 つ目の前半");
    assert.ok(resumed.includes("議事録を作成しています…"), resumed);
    assert.ok(!resumed.includes("1 つ目の後半"));
    first.open();
    // 終わると、完成した議事録に切り替わる
    await session.waitForText(finished, "1 つ目の後半");

    // 2 つ目: 作成中に別のノートへ移り、作成が終わってから戻る
    await openNote(session, "終わってから戻る");
    await pressBarButton(session, "議事録を作成");
    await session.waitForText(streaming, "2 つ目の前半");
    await openNote(session, "ほかのノート");
    second.open();
    // 別のノートを開いているあいだに作成が終わり、保存される（一覧のプレビューが議事録の内容に変わる）
    await session.waitForText('nav[aria-label="ノート一覧"]', "2 つ目の前半");
    assert.equal(await session.exists(sel.toast), false);
    await openNote(session, "終わってから戻る");
    const done = await session.waitForText(finished, "2 つ目の後半");
    assert.ok(done.includes("2 つ目の前半"), done);
    assert.equal(ctx.server.messages.length, 2, "戻ったときに作り直していない");
  } finally {
    first.open();
    second.open();
  }
});
