// AI チャット: 回答のストリーミング表示と、履歴の保存。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";
import { createGate } from "../lib/mock-server.mjs";
import { createNote, dismissToast, sel, toastText } from "../lib/ui.mjs";
import { Key } from "../lib/webdriver.mjs";

const chatInput = 'textarea[aria-label="AI への質問"]';
const openChat = (session) => session.click('xpath://*[@aria-label="録音"]//button[contains(normalize-space(.), "AI チャット")]');

/** チャットのパネルに出ているメッセージ: [{ from: "user" | "assistant", text }] */
async function chatMessages(session) {
  return session.execute(`
    return Array.from(document.querySelectorAll(".chat-list .chat-message")).map((m) => ({
      from: m.classList.contains("from-user") ? "user" : "assistant",
      text: m.innerText.trim(),
    }));
  `);
}

test("[CHT-003][CHT-008][NOTE-019] AI チャットで質問すると回答がストリーミングで表示され、履歴が残る", async (ctx) => {
  // 文字が届く前と、前半を送ったところで止めておき、途中の表示を確かめてから残りを送る
  const before = createGate();
  const gate = createGate();
  ctx.server.anthropic.reply = () => ({
    before: before.promise,
    chunks: ["リリースは ", "12/5 に"],
    hold: gate.promise,
    tail: ["決まりました。"],
  });
  let session = await ctx.launch();
  await createNote(session, { title: "週次定例", memo: "- リリースは 12/5" });

  await openChat(session);
  await session.waitForText('aside[aria-label="AI チャット"]', "このノートのメモ・文字起こし・議事録をもとに、AI が答えます。");
  await (await session.$(chatInput)).type("決まったことは？");
  await session.click('button[aria-label="送信"]');

  // 送った直後: 入力欄が空になり、質問と「考えています…」が出る
  await session.waitUntil(async () => (await chatMessages(session)).at(-1)?.text === "考えています…", {
    message: "回答を待つあいだの表示",
  });
  assert.deepEqual((await chatMessages(session))[0], { from: "user", text: "決まったことは？" });
  assert.equal(await (await session.$(chatInput)).value(), "", "送ったあと、入力欄が空になっていない");
  before.open();
  // 回答の途中: 質問が出て、回答は届いたところまで
  await session.waitUntil(async () => (await chatMessages(session)).at(-1)?.text === "リリースは 12/5 に", {
    message: "回答が途中まで表示される",
  });
  assert.equal((await chatMessages(session))[0].text, "決まったことは？");
  gate.open();
  // 完了
  await session.waitUntil(async () => (await chatMessages(session)).at(-1)?.text === "リリースは 12/5 に決まりました。", {
    message: "回答が最後まで表示される",
  });
  assert.deepEqual(await chatMessages(session), [
    { from: "user", text: "決まったことは？" },
    { from: "assistant", text: "リリースは 12/5 に決まりました。" },
  ]);

  // API に送った内容: 会議の情報はシステムプロンプト、質問は user のメッセージ
  const first = ctx.server.messages[0].json;
  assert.ok(first.system[0].text.includes("- リリースは 12/5"), "メモがシステムプロンプトに含まれる");
  assert.deepEqual(first.messages, [{ role: "user", content: "決まったことは？" }]);

  // 2 つ目の質問（Enter で送信）には、それまでの会話が付く
  ctx.server.anthropic.reply = () => ({ chunks: ["田中さんです。"] });
  await (await session.$(chatInput)).type(`担当は？${Key.Enter}`);
  await session.waitUntil(async () => (await chatMessages(session)).at(-1)?.text === "田中さんです。", {
    message: "2 つ目の回答が表示される",
  });
  assert.equal((await chatMessages(session)).length, 4);
  assert.deepEqual(ctx.server.messages[1].json.messages, [
    { role: "user", content: "決まったことは？" },
    { role: "assistant", content: "リリースは 12/5 に決まりました。" },
    { role: "user", content: "担当は？" },
  ]);

  // 再起動しても履歴が残っている
  session = await ctx.relaunch();
  await openChat(session);
  await session.waitUntil(async () => (await chatMessages(session)).length === 4, { message: "再起動後も履歴がある" });
  assert.equal((await chatMessages(session))[3].text, "田中さんです。");

  // 履歴の消去（確認つき）
  ctx.work.answerDialogs({ confirm: true });
  await session.click('button[aria-label="チャットの履歴を消去"]');
  await session.waitUntil(async () => (await chatMessages(session)).length === 0, { message: "履歴が消える" });
  assert.match(ctx.work.dialogs().at(-1).message, /AI チャットの履歴を消去しますか？/);
});

test("[CHT-005][ERR-002] AI チャットの API がエラーを返したら、メッセージが出て、質問は入力欄に戻る", async (ctx) => {
  ctx.server.anthropic.reply = () => ({ status: 401 });
  const session = await ctx.launch();
  await createNote(session, { title: "週次定例", memo: "- メモ" });
  await openChat(session);
  await (await session.$(chatInput)).type("決まったことは？");
  await session.click('button[aria-label="送信"]');

  await toastText(session, "Anthropic の API キーが正しくないか、権限がありません（HTTP 401");
  await session.waitUntil(async () => (await (await session.$(chatInput)).value()) === "決まったことは？", {
    message: "送れなかった質問が入力欄に戻る",
  });
  // 回答をもらえなかった質問は、履歴に残さない
  assert.deepEqual(await chatMessages(session), []);
  await dismissToast(session);

  // 直ったら、同じ質問をそのまま送り直せる
  ctx.server.anthropic.reply = () => ({ chunks: ["回答です。"] });
  await session.click('button[aria-label="送信"]');
  await session.waitUntil(async () => (await chatMessages(session)).at(-1)?.text === "回答です。", {
    message: "回答が表示される",
  });
  assert.equal(await session.exists(sel.toast), false);
  // 送り直しても、同じ質問が 2 つ並ばない。AI にも、質問は 1 回だけ渡す
  assert.deepEqual(await chatMessages(session), [
    { from: "user", text: "決まったことは？" },
    { from: "assistant", text: "回答です。" },
  ]);
  assert.deepEqual(ctx.server.messages.at(-1).json.messages, [{ role: "user", content: "決まったことは？" }]);
  // パネルを開き直しても同じ（履歴として保存されているのは、この 2 つだけ）
  await session.click('button[aria-label="パネルを閉じる"]');
  await openChat(session);
  await session.waitUntil(async () => (await chatMessages(session)).length === 2, { message: "履歴は 2 件" });
});

test("[CHT-006] 回答の作成は途中で中止でき、中止した質問と回答は履歴に残らない", async (ctx) => {
  const gate = createGate();
  // 前半を送ったあと、AI から何も届かない状態にする（中止は、次のデータを待たずに効く）
  ctx.server.anthropic.reply = () => ({ chunks: ["書きかけの"], hold: gate.promise, silent: true });
  try {
    const session = await ctx.launch();
    await createNote(session, { title: "週次定例", memo: "- メモ" });
    await openChat(session);
    await (await session.$(chatInput)).type("決まったことは？");
    await session.click('button[aria-label="送信"]');
    await session.waitUntil(async () => (await chatMessages(session)).at(-1)?.text === "書きかけの", {
      message: "回答が途中まで表示される",
    });

    await session.click('xpath://aside[@aria-label="AI チャット"]//button[normalize-space(.)="中止"]');
    // すぐに送信できる状態に戻り、質問は入力欄に戻る。エラーとしては知らせない
    await session.$('button[aria-label="送信"]', { timeout: 3_000 });
    assert.equal(await (await session.$(chatInput)).value(), "決まったことは？");
    assert.equal(await session.exists(sel.toast), false, "中止はエラーとして知らせない");
    assert.deepEqual(await chatMessages(session), [], "中止した質問や、書きかけの回答が残っている");
  } finally {
    gate.open();
  }
});

test("[CHT-014] 回答の作成中にパネルを閉じて開き直しても、作成中の表示に戻り、終わると回答が出る", async (ctx) => {
  const gate = createGate();
  ctx.server.anthropic.reply = () => ({ chunks: ["リリースは "], hold: gate.promise, tail: ["12/5 です。"] });
  try {
    const session = await ctx.launch();
    await createNote(session, { title: "週次定例", memo: "- メモ" });
    await openChat(session);
    await (await session.$(chatInput)).type("決まったことは？");
    await session.click('button[aria-label="送信"]');
    await session.waitUntil(async () => (await chatMessages(session)).at(-1)?.text === "リリースは", {
      message: "回答が途中まで表示される",
    });

    // 作成中にパネルを閉じて、開き直す
    await session.click('button[aria-label="パネルを閉じる"]');
    await session.waitGone('aside[aria-label="AI チャット"]');
    await openChat(session);
    // 質問が 1 つと、それまでに届いた回答が出て、中止もできる
    await session.waitUntil(
      async () => JSON.stringify(await chatMessages(session)) === JSON.stringify([
        { from: "user", text: "決まったことは？" },
        { from: "assistant", text: "リリースは" },
      ]),
      { message: "開き直すと、作成中の表示に戻る" },
    );
    await session.$('xpath://aside[@aria-label="AI チャット"]//button[normalize-space(.)="中止"]');
    assert.equal(await session.exists('button[aria-label="送信"]'), false);

    // 終わると、そのまま回答に切り替わる
    gate.open();
    await session.waitUntil(async () => (await chatMessages(session)).at(-1)?.text === "リリースは 12/5 です。", {
      message: "回答が最後まで表示される",
    });
    await session.$('button[aria-label="送信"]');
    assert.equal((await chatMessages(session)).length, 2);
    assert.equal(ctx.server.messages.length, 1, "開き直したときに、送り直していない");
  } finally {
    gate.open();
  }
});
