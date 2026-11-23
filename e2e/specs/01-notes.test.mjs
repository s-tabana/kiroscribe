// ノートの作成・編集・検索・削除と、再起動後も残っていること。

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "../lib/app.mjs";
import { barButton, createNote, noteTitles, openNote, pressBarButton, sel, utterances, waitSaved } from "../lib/ui.mjs";
import { Key } from "../lib/webdriver.mjs";

const noteList = 'nav[aria-label="ノート一覧"]';

test("[NOTE-001][NOTE-008][NOTE-009][NOTE-014][NOTE-019][NOTE-020][DAT-001][TRY-007] 起動するとメイン画面が出て、ノートを作成・編集・検索・削除でき、再起動後も残っている", async (ctx) => {
  let session = await ctx.launch();

  // ふつうに起動すると、メインウィンドウが決まった大きさで表示される
  assert.equal(await session.title(), "Kiroscribe");
  assert.equal(await ctx.app.isMainVisible(), true);
  const innerSize = () => session.execute("return [window.innerWidth, window.innerHeight]");
  const [screenWidth, screenHeight] = await session.execute("return [screen.availWidth, screen.availHeight]");
  if (screenWidth >= 1240 && screenHeight >= 820) {
    assert.deepEqual(await innerSize(), [1240, 820], "メインウィンドウの大きさ");
  }
  // 幅 860px・高さ 560px より小さくはできない（それより小さくしようとしても、860x560 で止まる）
  ctx.work.resizeWindow("main", 400, 300);
  await session.waitUntil(async () => (await innerSize()).join("x") === "860x560", {
    message: `ウィンドウが最小の大きさ（860x560）になる（いまは ${(await innerSize()).join("x")}）`,
  });
  // 最小より大きい大きさには、そのまま変えられる
  ctx.work.resizeWindow("main", 900, 600);
  await session.waitUntil(async () => (await innerSize()).join("x") === "900x600", { message: "ウィンドウが 900x600 になる" });

  // 初回起動: ノートはまだない。データベースは、アプリのデータフォルダの中にできる
  await session.waitForText("main", "最初の会議ノートを作りましょう");
  await session.waitForText(noteList, "まだノートがありません");
  assert.ok(fs.existsSync(path.join(ctx.work.dataDir, "kiroscribe.db")), "データフォルダに kiroscribe.db がない");

  // 作成: 空のノートが一覧の先頭に加わり、開いて、タイトル欄にカーソルが入る（createNote が確かめる）。
  // 入力は自動で保存され、保存されると一覧のタイトルとプレビューが変わる
  await createNote(session, { title: "週次定例", memo: "- リリース日を決める" });
  await session.click(sel.newNote);
  await session.waitUntil(async () => (await noteTitles(session)).join() === "無題のノート,週次定例", {
    message: "新しいノートが、空のタイトルで一覧の先頭に加わる",
  });
  await (await session.$(sel.title)).type("採用面談");
  await session.waitForText(sel.saveState, "保存しました");
  await session.waitUntil(async () => (await noteTitles(session)).join() === "採用面談,週次定例", {
    message: "保存されると、一覧のタイトルが変わる",
  });
  await (await session.$(sel.memo)).type("候補者 A と話した");
  await waitSaved(session);
  await session.waitForText(`${sel.noteItems}[aria-current="page"]`, "候補者 A と話した", { timeout: 5_000 });

  // 検索（タイトルと、プレビューに出ている本文の先頭）
  const search = await session.$(sel.search);
  await search.type("リリース");
  await session.waitUntil(async () => (await noteTitles(session)).join() === "週次定例", { message: "検索で絞り込まれる" });
  await search.fill("どこにもない言葉");
  await session.waitForText(noteList, "一致するノートがありません");
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
  assert.equal(
    ctx.work.dialogs()[0].message,
    "このノートを削除しますか？\nメモ・文字起こし・議事録もすべて削除され、元に戻せません。",
  );
  assert.equal(ctx.work.dialogs()[0].title, "ノートの削除");
  assert.equal(ctx.work.dialogs()[0].kind, "warning");
  assert.equal((await noteTitles(session)).length, 2);
  assert.equal(await (await session.$(sel.title)).value(), "採用面談", "取り消したら、開いているノートもそのまま");

  // 「はい」なら消えて、一覧で次にあったノートが開く
  ctx.work.answerDialogs({ confirm: true });
  await session.click('button[aria-label="ノートを削除"]');
  await session.waitUntil(async () => (await noteTitles(session)).join() === "週次定例（11 月）", {
    message: "削除したノートが一覧から消える",
  });
  await session.waitUntil(async () => (await (await session.$(sel.title)).value()) === "週次定例（11 月）", {
    message: "削除したあと、次のノートが開く",
  });

  // 再起動しても残っている。起動すると、いちばん新しいノートが開く
  await createNote(session, { title: "あとから作ったノート", memo: "新しいメモ" });
  session = await ctx.relaunch();
  await session.waitUntil(async () => (await noteTitles(session)).join() === "あとから作ったノート,週次定例（11 月）", {
    message: "再起動後もノートがある",
  });
  assert.equal(await (await session.$(sel.title)).value(), "あとから作ったノート");
  assert.equal(await (await session.$(sel.memo)).value(), "新しいメモ");
  await openNote(session, "週次定例（11 月）");
  assert.equal(await (await session.$(sel.memo)).value(), "- リリース日を決める\n- 担当を決める");
});

test("[NOTE-001][NOTE-002][NOTE-010] 新しいノートは Ctrl+N でも作れ、検索は空に戻る。書きかけの入力は、ノートを切り替えるときに保存される", async (ctx) => {
  let session = await ctx.launch();
  await createNote(session, { title: "一つ目", memo: "最初のメモ" });

  // 検索欄に入力があるとき、新しいノートを作ると、検索は空に戻る（作ったノートが絞り込みで隠れない）
  const search = await session.$(sel.search);
  await search.type("一つ目");
  await session.waitUntil(async () => (await noteTitles(session)).join() === "一つ目", { message: "検索で絞り込まれる" });
  await session.click(sel.newNote);
  await session.waitUntil(async () => (await noteTitles(session)).join() === "無題のノート,一つ目", {
    message: "新しいノートが一覧の先頭に出る",
  });
  assert.equal(await (await session.$(sel.search)).value(), "", "検索欄が空に戻っていない");
  await (await session.$(sel.title)).type("二つ目");
  await waitSaved(session);

  // Ctrl+N でも、同じように新しいノートができて、タイトル欄にカーソルが入る
  await (await session.$(sel.memo)).type(`${Key.Control}n`);
  await session.waitUntil(async () => (await noteTitles(session)).join() === "無題のノート,二つ目,一つ目", {
    message: "Ctrl+N で新しいノートができる",
  });
  await session.waitUntil(
    () => session.execute(`const t = document.querySelector(${JSON.stringify(sel.title)}); return t.value === "" && document.activeElement === t`),
    { message: "新しいノートのタイトル欄にカーソルが入る" },
  );
  assert.equal(await (await session.$(sel.memo)).value(), "", "Ctrl+N の n が、メモに入力されている");
  await (await session.$(sel.title)).type("三つ目");
  await waitSaved(session);

  // 書きかけの入力は、0.5 秒の自動保存を待たずに別のノートへ切り替えても、保存される
  await openNote(session, "一つ目");
  await (await session.$(sel.memo)).type(" と、切り替える直前に書いた分");
  await session.click(`xpath://nav[@aria-label="ノート一覧"]//button[.//span[normalize-space(.)="二つ目"]]`);
  await session.waitUntil(async () => (await (await session.$(sel.title)).value()) === "二つ目", { message: "ノートが切り替わる" });
  await session.waitForText(noteList, "最初のメモ と、切り替える直前に書いた分");
  session = await ctx.relaunch();
  await openNote(session, "一つ目");
  assert.equal(await (await session.$(sel.memo)).value(), "最初のメモ と、切り替える直前に書いた分");
});

test("[NOTE-008] 検索は、メモの本文・議事録・文字起こしに含まれる言葉でもノートを見つける", async (ctx) => {
  ctx.server.anthropic.reply = () => ({
    chunks: ["# 採用面談\n\n", "## 決定事項\n", "- 一次面接は通過\n", "- 二次面接は箱根で行う\n"],
  });
  const session = await ctx.launch();
  const search = await session.$(sel.search);
  /** 検索欄に入れて、一覧がその並びになるまで待つ（一覧のプレビューに出ない言葉は、本体に問い合わせた結果で加わる） */
  const expectFound = async (query, titles, message) => {
    await search.fill(query);
    await session.waitUntil(async () => (await noteTitles(session)).join() === titles.join(), { message });
  };

  // 文字起こしにだけ言葉があるノート（メモは空なので、一覧のプレビューには何も出ない）
  await createNote(session, { title: "録音のノート" });
  await pressBarButton(session, "録音を開始");
  await session.waitUntil(async () => (await utterances(session)).length > 0, {
    timeout: 30_000,
    message: "発話が表示される",
  });
  await pressBarButton(session, "停止");
  await session.$(barButton("録音を再開"), { timeout: 30_000 });
  assert.match((await utterances(session))[0].text, /^模擬の文字起こし \d+。/);

  // 議事録の 2 行目にだけ言葉があるノート（プレビューに出るのは 1 行目の「一次面接は通過」）
  await createNote(session, { title: "採用面談", memo: "- 候補者 A" });
  await pressBarButton(session, "議事録を作成");
  await session.waitForText(`${sel.minutes}[aria-busy="false"]`, "二次面接は箱根で行う");

  // メモの 2 行目にだけ言葉があるノート（プレビューに出るのは 1 行目の「日程」）
  await createNote(session, { title: "週次定例", memo: "- 日程\n- 来月の Offsite の場所" });
  assert.deepEqual(await noteTitles(session), ["週次定例", "採用面談", "録音のノート"]);

  await expectFound("来月", ["週次定例"], "メモの本文で見つかる");
  await expectFound("offsite", ["週次定例"], "大文字と小文字は区別しない");
  await expectFound("箱根", ["採用面談"], "議事録で見つかる");
  await expectFound("模擬の文字起こし", ["録音のノート"], "文字起こしで見つかる");
  // タイトルやプレビューで見つかるノートと、本体に問い合わせて見つかるノートは、一覧の並びのまま一緒に出る
  await expectFound("面接", ["採用面談"], "プレビューと議事録の両方に当てはまるノートは 1 回だけ出る");
  await search.fill("どこにもない言葉");
  await session.waitForText(noteList, "一致するノートがありません");
  await search.fill("");
  await session.waitUntil(async () => (await noteTitles(session)).length === 3, { message: "検索を消すと全件に戻る" });
});
