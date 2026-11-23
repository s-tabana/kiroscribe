// ノートの作成・編集・検索・削除と、再起動後も残っていること。

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "../lib/app.mjs";
import { barButton, closeSettings, createNote, noteTitles, openNote, openSettings, pressBarButton, sel, utterances, waitSaved } from "../lib/ui.mjs";
import { Key } from "../lib/webdriver.mjs";

const noteList = 'nav[aria-label="ノート一覧"]';

test("[NOTE-001][NOTE-008][NOTE-009][NOTE-014][NOTE-019][NOTE-020][DAT-001] 起動するとメイン画面が出て、ノートを作成・編集・検索・削除でき、再起動後も残っている", async (ctx) => {
  let session = await ctx.launch();

  // ふつうに起動すると、メインウィンドウが表示される
  assert.equal(await session.title(), "Kiroscribe");
  assert.equal(await ctx.app.isMainVisible(), true);

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

test("[TRY-007] ふつうに起動するとメインウィンドウが 1240x820 で表示され、860x560 より小さくはできない", async (ctx) => {
  const session = await ctx.launch();
  assert.equal(await ctx.app.isMainVisible(), true);
  // 始まりの大きさ（画面が小さくて収まらないときは、OS が画面の大きさまで縮める）
  const [screenWidth, screenHeight] = await session.execute("return [screen.availWidth, screen.availHeight]");
  if (screenWidth >= 1240 && screenHeight >= 820) {
    assert.deepEqual(await session.execute("return [window.innerWidth, window.innerHeight]"), [1240, 820]);
  }

  // 大きさは、ウィンドウそのものに問い合わせる（アプリの側から変えて、変えたあとの値を受け取る）。
  // まず、最小より大きい大きさに変えられること（大きさを変える操作が効いていること）
  const resized = await ctx.work.resizeWindow("main", 900, 600);
  assert.deepEqual(resized.problems, [], JSON.stringify(resized));
  assert.deepEqual(resized.inner, [900, 600], `900x600 にならない: ${JSON.stringify(resized)}`);
  if (process.platform === "win32") {
    // Windows は、プログラムからの大きさの変更には最小の大きさを当てはめない（利用者がウィンドウの端をドラッグしたときだけ、
    // ウィンドウに最小の大きさを問い合わせて止める）。そこで、同じ問い合わせの答えが、内側の 860x560 に当たることを確かめる
    // （利用者のドラッグは、OS がこの答えで止める。その大きさで表示が崩れないことは、下のテストで確かめる）
    const min = resized.minSize;
    // 問い合わせの答えそのまま（track）も、ログに残す
    console.log(`[e2e] メインウィンドウの最小の大きさ（問い合わせの答え）: ${JSON.stringify(min)}`);
    assert.ok(min && !min.problem, `最小の大きさを問い合わせられない: ${JSON.stringify(resized)}`);
    // 枠の大きさの端数（拡大率が 100% でないとき）を見込んで、1px の差は許す
    const near = (actual, expected) => Math.abs(actual - expected) <= 1;
    assert.ok(near(min.inner[0], 860) && near(min.inner[1], 560), `最小の大きさが 860x560 でない: ${JSON.stringify(resized)}`);
    return;
  }
  // 最小（幅 860px・高さ 560px）より小さくしようとしても、最小で止まる
  const shrunk = await ctx.work.resizeWindow("main", 400, 300);
  assert.deepEqual(shrunk.inner, [860, 560], `最小の大きさで止まらない: ${JSON.stringify(shrunk)}`);
  // 幅だけ・高さだけを小さくしようとしても同じ
  const narrow = await ctx.work.resizeWindow("main", 400, 700);
  assert.deepEqual(narrow.inner, [860, 700], JSON.stringify(narrow));
  const low = await ctx.work.resizeWindow("main", 1000, 300);
  assert.deepEqual(low.inner, [1000, 560], JSON.stringify(low));
  // 画面（ウェブビュー）も、ウィンドウの大きさに追従している
  await session.waitUntil(async () => (await session.execute("return [window.innerWidth, window.innerHeight]")).join("x") === "1000x560", {
    message: "画面の大きさが、ウィンドウの大きさに合う",
  });
});

/**
 * いま見えている画面の部品が、ウィンドウからはみ出していないかを調べる（はみ出しの一覧を返す）。
 * - 画面全体が横にはみ出して、横のスクロールができていない
 * - ボタン・入力欄・タブが、ウィンドウの左右からはみ出していない
 * - 決まった部品（required）が、上下も含めてすべて見えている
 */
function layoutProblems(session, required) {
  return session.execute(`
    const vw = document.documentElement.clientWidth;
    const vh = document.documentElement.clientHeight;
    const problems = [];
    const describe = (el) => (el.getAttribute("aria-label") || el.textContent || el.tagName).trim().slice(0, 30);
    if (document.documentElement.scrollWidth > vw + 1) {
      problems.push("画面全体が横にはみ出している: " + document.documentElement.scrollWidth + " > " + vw);
    }
    for (const el of document.querySelectorAll('button, input, textarea, select, [role="tab"]')) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0 || getComputedStyle(el).visibility === "hidden") continue;
      if (r.left < -1 || r.right > vw + 1) problems.push("横にはみ出している: " + describe(el) + "（" + Math.round(r.left) + "〜" + Math.round(r.right) + "）");
    }
    for (const selector of ${JSON.stringify(required)}) {
      const el = document.querySelector(selector);
      if (!el) { problems.push("見つからない: " + selector); continue; }
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) problems.push("大きさがない: " + selector);
      else if (r.top < -1 || r.bottom > vh + 1 || r.left < -1 || r.right > vw + 1) {
        problems.push("ウィンドウに収まっていない: " + selector + "（" + [r.left, r.top, r.right, r.bottom].map(Math.round).join(",") + "、ウィンドウ " + vw + "x" + vh + "）");
      }
    }
    return problems;
  `);
}

test("[TRY-007] 最小の大きさ（860x560）でも、画面の部品がはみ出さず、主な操作がすべて見えている", async (ctx) => {
  const session = await ctx.launch();
  // 長いタイトルのノートと、録音のバーを出しておく（いちばん横幅を使う状態）
  await createNote(session, { title: "とても長いタイトルの会議メモ".repeat(6), memo: "メモ" });
  const resized = await ctx.work.resizeWindow("main", 860, 560);
  assert.deepEqual(resized.inner, [860, 560], JSON.stringify(resized));
  await session.waitUntil(async () => (await session.execute("return [window.innerWidth, window.innerHeight]")).join("x") === "860x560", {
    message: "画面の大きさが 860x560 になる",
  });

  const main = [sel.newNote, sel.search, sel.title, sel.memo, `${sel.recordingBar} button`];
  assert.deepEqual(await layoutProblems(session, main), []);

  // 設定画面も、ウィンドウに収まり、保存と閉じるのボタンが見えている（中身はスクロールして読む）
  await openSettings(session);
  const footer = `${sel.settingsDialog} .modal-footer button`;
  assert.deepEqual(await layoutProblems(session, [sel.settingsDialog, footer]), []);
  await closeSettings(session);

  // ヘルプも同じ
  await session.click('button[aria-label="ヘルプ"]');
  await session.$(sel.helpDialog);
  assert.deepEqual(await layoutProblems(session, [sel.helpDialog, `${sel.helpDialog} button[aria-label="閉じる"]`]), []);
});
