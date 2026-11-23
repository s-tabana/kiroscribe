// ノートの作成・編集・検索・削除と、再起動後も残っていること。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";
import { createNote, noteTitles, openNote, sel, waitSaved } from "../lib/ui.mjs";

test("起動するとメイン画面が出て、ノートを作成・編集・検索・削除でき、再起動後も残っている", async (ctx) => {
  let session = await ctx.launch();

  // 初回起動: ノートはまだない
  assert.equal(await session.title(), "Kiroscribe");
  await session.waitForText("main", "最初の会議ノートを作りましょう");
  await session.waitForText('nav[aria-label="ノート一覧"]', "まだノートがありません");

  // 作成（入力は自動で保存される）
  await createNote(session, { title: "週次定例", memo: "- リリース日を決める" });
  await createNote(session, { title: "採用面談", memo: "候補者 A と話した" });
  assert.deepEqual(await noteTitles(session), ["採用面談", "週次定例"], "新しいノートが上");
  await session.waitForText(`${sel.noteItems}[aria-current="page"]`, "候補者 A と話した", { timeout: 5_000 });

  // 検索（タイトルと本文の先頭）
  const search = await session.$(sel.search);
  await search.type("リリース");
  await session.waitUntil(async () => (await noteTitles(session)).join() === "週次定例", { message: "検索で絞り込まれる" });
  await search.fill("どこにもない言葉");
  await session.waitForText('nav[aria-label="ノート一覧"]', "一致するノートがありません");
  await search.fill("");
  await session.waitUntil(async () => (await noteTitles(session)).length === 2, { message: "検索を消すと全件に戻る" });

  // 編集
  await openNote(session, "週次定例");
  await (await session.$(sel.title)).fill("週次定例（11 月）");
  await (await session.$(sel.memo)).type("\n- 担当を決める");
  await waitSaved(session);
  await session.waitUntil(async () => (await noteTitles(session)).includes("週次定例（11 月）"), {
    message: "一覧のタイトルが変わる",
  });

  // 削除: 確認で「いいえ」なら残る
  await openNote(session, "採用面談");
  ctx.work.answerDialogs({ confirm: false });
  await session.click('button[aria-label="ノートを削除"]');
  await session.waitUntil(() => ctx.work.dialogs().length === 1, { message: "削除の確認が出る" });
  assert.match(ctx.work.dialogs()[0].message, /このノートを削除しますか？/);
  assert.equal(ctx.work.dialogs()[0].title, "ノートの削除");
  assert.equal(ctx.work.dialogs()[0].kind, "warning");
  assert.equal((await noteTitles(session)).length, 2);

  // 「はい」なら消える
  ctx.work.answerDialogs({ confirm: true });
  await session.click('button[aria-label="ノートを削除"]');
  await session.waitUntil(async () => (await noteTitles(session)).join() === "週次定例（11 月）", {
    message: "削除したノートが一覧から消える",
  });

  // 再起動しても残っている
  session = await ctx.relaunch();
  await session.waitUntil(async () => (await noteTitles(session)).join() === "週次定例（11 月）", {
    message: "再起動後もノートがある",
  });
  assert.equal(await (await session.$(sel.title)).value(), "週次定例（11 月）");
  assert.equal(await (await session.$(sel.memo)).value(), "- リリース日を決める\n- 担当を決める");
});
