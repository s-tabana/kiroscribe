// 手動の録音: 模擬の音声が「自分」「相手」として文字起こしされ、一時停止・停止できる。

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "../lib/app.mjs";
import { describeWav } from "../lib/mock-server.mjs";
import {
  barButton,
  createNote,
  dismissToast,
  noteTitles,
  openNote,
  openTranscript,
  pressBarButton,
  sel,
  toastText,
  utteranceCount,
  utterances,
} from "../lib/ui.mjs";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const timer = `${sel.recordingBar} [role="timer"]`;
const transcriptList = ".transcript-list";

test("[REC-001][REC-009][REC-015][TRN-016][TRN-024][NOTE-019][SEC-011] 録音を開始すると文字起こしが「自分」「相手」で表示され、停止・再開でき、再起動後も残る。録音中のノートは削除できない", async (ctx) => {
  let session = await ctx.launch();
  await createNote(session, { title: "録音のテスト" });

  await pressBarButton(session, "録音を開始");
  // 状態が「録音中」になり、経過時間が進む
  const first = await session.waitForText(timer, "録音中");
  await session.waitUntil(async () => (await session.text(timer)) !== first, { message: "経過時間が進む" });
  // 文字起こしのパネルが開き、一覧には録音中の印が出る
  await session.$('aside[aria-label="文字起こし"]');
  await session.$(`${sel.noteItems} [aria-label="録音中"]`);

  // 模擬の音声（発話に似た合成音）が区切られて、文字起こしの API に送られ、結果が話者ごとに出る
  await session.waitUntil(
    async () => {
      const speakers = new Set((await utterances(session)).map((u) => u.speaker));
      return speakers.has("自分") && speakers.has("相手");
    },
    { timeout: 30_000, message: "自分と相手の発話が表示される" },
  );
  for (const utterance of await utterances(session)) {
    assert.match(utterance.text, /^模擬の文字起こし \d+。/);
  }

  // 録音中のノートは削除できない
  ctx.work.answerDialogs({ confirm: true });
  await session.click('button[aria-label="ノートを削除"]');
  await toastText(session, "録音中のノートは削除できません。先に録音を停止してください");
  await dismissToast(session);
  assert.deepEqual(await noteTitles(session), ["録音のテスト"]);

  // 一時停止と再開
  await pressBarButton(session, "一時停止");
  await session.waitForText(`${sel.recordingBar} [role="timer"]`, "一時停止中");
  await pressBarButton(session, "再開");
  await session.waitForText(`${sel.recordingBar} [role="timer"]`, "録音中");

  // 録音中に別のノートを開いても、録音は続く。そのノートには、録音中の発話を表示しない
  await createNote(session, { title: "別のノート" });
  await session.waitForText(sel.recordingBar, "別のノートで録音中");
  const sentBefore = ctx.server.transcriptions.length;
  await session.waitUntil(() => ctx.server.transcriptions.length > sentBefore, { timeout: 30_000, message: "録音は続いている" });
  assert.equal(await utteranceCount(session), 0, "ほかのノートの発話が表示されている");
  await session.click(barButton("そのノートを開く"));
  await session.waitUntil(async () => (await (await session.$(sel.title)).value()) === "録音のテスト", {
    message: "録音中のノートに戻る",
  });

  // 停止: 残りの文字起こしがすべて終わるまで「文字起こしを仕上げています…」と表示し、終わると録音していない状態に戻る
  ctx.server.openai.delayMs = 1_500;
  await pressBarButton(session, "停止");
  await session.waitForText(sel.recordingBar, "文字起こしを仕上げています…");
  await session.$(barButton("録音を再開"), { timeout: 30_000 });
  ctx.server.openai.delayMs = 0;
  assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false);
  const shown = await utterances(session);
  assert.ok(shown.length >= 2, "停止後も文字起こしが残っている");
  // 届いた発話は 1 回ずつ表示される（文字起こしの API が返した数と同じ）
  const afterFirstRecording = await utteranceCount(session);
  assert.equal(afterFirstRecording, ctx.server.transcriptions.length, "発話の数が、文字起こしの数と合わない");
  const firstText = shown[0].text.split("\n")[0];

  // API に送った内容: 16kHz・モノラルの WAV、モデルと言語の指定
  assert.ok(ctx.server.transcriptions.length >= 2);
  for (const request of ctx.server.transcriptions) {
    const { fields, files } = request.multipart;
    assert.equal(fields.model, "gpt-4o-transcribe");
    assert.equal(fields.language, "ja");
    assert.equal(fields.response_format, "json");
    assert.equal(files.file.filename, "audio.wav");
    const wav = describeWav(files.file.data);
    assert.deepEqual([wav.channels, wav.sampleRate, wav.bitsPerSample], [1, 16_000, 16]);
    // 停止のときに話している途中だった発話は、短いまま送られる（声の部分が 0.4 秒に満たない音は送らない）
    assert.ok(wav.seconds >= 0.4 && wav.seconds <= 26, `発話の長さ: ${wav.seconds} 秒`);
  }
  // 2 回目以降は、直前の発話を文脈として渡す
  assert.ok(ctx.server.transcriptions.some((request) => request.multipart.fields.prompt?.includes("模擬の文字起こし")));

  // 音声は、文字起こしのために OpenAI へ送るだけ: ほかの宛先には何も送らず、ファイルとしても保存しない
  const withBody = ctx.server.requests.filter((request) => request.body.length > 0);
  assert.ok(withBody.length > 0);
  assert.deepEqual([...new Set(withBody.map((request) => request.path))], ["/openai/v1/audio/transcriptions"]);
  for (const file of ctx.work.dataFiles()) {
    assert.doesNotMatch(path.basename(file), /\.(wav|mp3|m4a|ogg|webm|flac|pcm)$/i, "音声のファイルが保存されている");
    const head = Buffer.alloc(12);
    let fd;
    try {
      fd = fs.openSync(file, "r");
      fs.readSync(fd, head, 0, 12, 0);
    } catch (err) {
      // ウェブビューが排他的に開いているファイル（Windows）は読めない
      if (!["EBUSY", "EPERM", "EACCES"].includes(err.code)) throw err;
      continue;
    } finally {
      if (fd !== undefined) fs.closeSync(fd);
    }
    assert.notEqual(head.toString("latin1", 0, 4), "RIFF", `${file} が音声（WAV）のファイルになっている`);
  }

  // 文字起こしのあるノートでは、開始のボタンが「録音を再開」になる。押すと同じノートで録音が始まり、発話は後ろに加わる
  await pressBarButton(session, "録音を再開");
  await session.waitForText(timer, "録音中");
  await session.waitUntil(async () => (await utteranceCount(session)) > afterFirstRecording, {
    timeout: 30_000,
    message: "新しい発話が加わる",
  });
  assert.equal((await utterances(session))[0].text.split("\n")[0], firstText, "前の録音の発話が、先頭に残っている");
  // 議事録のないノートでは聞かずに続きとして録音し、再開した位置に区切りの見出しが入る
  assert.equal(await session.exists('div[role="dialog"]'), false, "議事録のないノートで、続きかを聞いている");
  assert.match(await session.text(".transcript-list .transcript-break"), /^2回目の録音（\d{1,2}:\d{2}〜）$/);
  assert.deepEqual(await noteTitles(session), ["別のノート", "録音のテスト"], "録音を再開しても、ノートは増えない");
  await pressBarButton(session, "停止");
  await session.$(barButton("録音を再開"), { timeout: 30_000 });
  const total = await utteranceCount(session);

  // 再起動しても、文字起こしは残っている
  session = await ctx.relaunch();
  await openNote(session, "録音のテスト");
  await openTranscript(session);
  await session.waitUntil(async () => (await utteranceCount(session)) === total, { message: "再起動後も文字起こしがある" });

  // 録音していたあいだに開いていた別のノートには、発話は入っていない
  await openNote(session, "別のノート");
  await openTranscript(session);
  await session.waitForText(".transcript-list", "録音を開始すると、会話の文字起こしがここに表示されます。");
  assert.equal(await utteranceCount(session), 0);
  await openNote(session, "録音のテスト");

  // 停止後は削除できる
  ctx.work.answerDialogs({ confirm: true });
  await session.click('button[aria-label="ノートを削除"]');
  await session.waitUntil(async () => (await noteTitles(session)).join() === "別のノート", { message: "ノートが消える" });
});

test("[TRN-019] 新しい発話が届いたとき、末尾を見ていれば自動でスクロールし、上を読んでいるときは動かさない", async (ctx) => {
  let session = await ctx.launch();
  await createNote(session, { title: "長い会議" });
  // 画面に収まらない数の発話を、あらかじめ入れておく（アプリを止めているあいだに、データベースへ直接入れる）
  await ctx.stop();
  await ctx.work.withDatabase((db) => {
    const { id } = db.prepare("SELECT id FROM notes").get();
    const insert = db.prepare(
      "INSERT INTO transcript_segments (note_id, speaker, started_at, ended_at, text) VALUES (?, ?, ?, ?, ?)",
    );
    const start = Date.now() - 3_600_000;
    for (let i = 0; i < 60; i += 1) {
      // 2 分おきにして、発話ごとに別のまとまりにする
      insert.run(id, i % 2 === 0 ? "me" : "them", start + i * 120_000, start + i * 120_000 + 5_000, `前の録音の発話 ${i + 1}`);
    }
  });
  session = await ctx.launch();

  await pressBarButton(session, "録音を再開");
  await session.$(transcriptList);
  const scroll = () =>
    session.execute(`
      const list = document.querySelector(${JSON.stringify(transcriptList)});
      return { top: list.scrollTop, fromBottom: list.scrollHeight - list.scrollTop - list.clientHeight, height: list.clientHeight, full: list.scrollHeight };
    `);
  await session.waitUntil(async () => (await utteranceCount(session)) >= 60, { message: "前の録音の発話が表示される" });
  const initial = await scroll();
  assert.ok(initial.full > initial.height * 2, `スクロールが要るだけの発話がない: ${JSON.stringify(initial)}`);

  // 末尾を見ているとき: 新しい発話が届くと、末尾までスクロールする
  await session.waitUntil(async () => (await scroll()).fromBottom < 2, { message: "開いたときは末尾を表示する" });
  let count = await utteranceCount(session);
  await session.waitUntil(async () => (await utteranceCount(session)) > count, { timeout: 30_000, message: "新しい発話が届く" });
  await session.waitUntil(async () => (await scroll()).fromBottom < 2, { message: "新しい発話に合わせて末尾までスクロールする" });

  // 上のほうを読んでいるとき: 新しい発話が届いても、スクロール位置を動かさない
  await session.execute(`document.querySelector(${JSON.stringify(transcriptList)}).scrollTop = 120`);
  await session.waitUntil(async () => (await scroll()).top === 120, { message: "上へスクロールする" });
  await sleep(200); // スクロールの知らせが届くまで
  count = await utteranceCount(session);
  await session.waitUntil(async () => (await utteranceCount(session)) > count + 1, { timeout: 30_000, message: "新しい発話が届く" });
  assert.equal((await scroll()).top, 120, "読んでいる位置が動いた");

  // 末尾の近く（下から 48px 以内）まで戻ると、また自動でスクロールする
  await session.execute(`
    const list = document.querySelector(${JSON.stringify(transcriptList)});
    list.scrollTop = list.scrollHeight - list.clientHeight - 30;
  `);
  await sleep(200);
  count = await utteranceCount(session);
  await session.waitUntil(async () => (await utteranceCount(session)) > count, { timeout: 30_000, message: "新しい発話が届く" });
  await session.waitUntil(async () => (await scroll()).fromBottom < 2, { message: "末尾の近くにいれば、末尾までスクロールする" });
  await pressBarButton(session, "停止");
  await session.$(barButton("録音を再開"), { timeout: 30_000 });
});

test(
  "[REC-018] 相手の声を取り込めないときは、マイクだけで録音を続けて、そのことを知らせる",
  { config: { audio: { mic: "speech", system: null } } },
  async (ctx) => {
    const session = await ctx.launch();
    await createNote(session, { title: "マイクだけ" });
    await pressBarButton(session, "録音を開始");
    await toastText(session, "相手の声は取り込まず、マイクだけで録音します");
    await session.waitForText('aside[aria-label="文字起こし"]', "相手の声は取り込んでいません（マイクのみ）");
    await session.waitUntil(async () => (await utterances(session)).length > 0, {
      timeout: 30_000,
      message: "自分の発話が表示される",
    });
    assert.ok((await utterances(session)).every((u) => u.speaker === "自分"));
    await pressBarButton(session, "停止");
    await session.$(barButton("録音を再開"), { timeout: 30_000 });
  },
);

test("[TRN-014] 文字起こしの API がキーを拒否したら、文字起こしと録音を止めて、キーの確認を促す", async (ctx) => {
  ctx.server.openai.apiKey = "sk-別のキー";
  const session = await ctx.launch();
  await createNote(session, { title: "キーの誤り" });
  await pressBarButton(session, "録音を開始");
  const message = await toastText(session, "文字起こしを停止しました。API キーを確認してください", { timeout: 30_000 });
  assert.match(message, /OpenAI の API キーが正しくないか、権限がありません（HTTP 401/);
  // 録音も止まる（「録音中」の表示と経過時間が進み続けない）
  await session.$(barButton("録音を開始"), { timeout: 30_000 });
  assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false);
  // 止まったあとは、音声を送らない。知らせも増えない
  const sent = ctx.server.transcriptions.length;
  await sleep(3_500);
  assert.equal(ctx.server.transcriptions.length, sent, "録音を止めたあとも、音声を送っている");
  assert.equal((await session.findAll(sel.toast)).length, 1);
  // キーに関する知らせには、設定画面への近道が付く
  await session.click(`${sel.toast} button.button`);
  await session.$(sel.settingsDialog);
});

test("[TRN-013] 文字起こしの API の一時的なエラー（429・500）は、再試行して成功すれば表に出さない", async (ctx) => {
  ctx.server.openai.failure = { status: 500, times: 1 };
  const session = await ctx.launch();
  await createNote(session, { title: "再試行" });
  await pressBarButton(session, "録音を開始");
  await session.waitUntil(async () => (await utterances(session)).length > 0, {
    timeout: 30_000,
    message: "再試行のあと発話が表示される",
  });
  ctx.server.openai.failure = { status: 429, times: 1 };
  const count = (await utterances(session)).length + ctx.server.transcriptions.length;
  await session.waitUntil(async () => (await utterances(session)).length + ctx.server.transcriptions.length > count + 2, {
    timeout: 30_000,
    message: "429 のあとも文字起こしが続く",
  });
  await pressBarButton(session, "停止");
  await session.$(barButton("録音を再開"), { timeout: 30_000 });
  assert.equal(await session.exists(sel.toast), false, "再試行で直ったエラーは知らせない");
});

test("[REC-027][TRN-024][MIN-021] 議事録のあるノートで録音を再開すると、同じ会議の続きか新しいノートかを聞き、続きなら区切りを付けて録音する", async (ctx) => {
  ctx.server.anthropic.reply = () => ({ chunks: ["# 再開のテスト\n\n", "## 決定事項\n", "- 一回目の内容\n"] });
  const session = await ctx.launch();
  await createNote(session, { title: "再開のテスト" });
  const dialog = 'div[role="dialog"][aria-labelledby="resume-title"]';
  const dialogButton = (text) => `xpath://div[@role="dialog"][@aria-labelledby="resume-title"]//button[normalize-space(.)="${text}"]`;
  const recordOnce = async (button) => {
    const before = await utteranceCount(session);
    await session.waitForText(timer, "録音中");
    await session.waitUntil(async () => (await utteranceCount(session)) > before, { timeout: 30_000, message: "発話が加わる" });
    await pressBarButton(session, "停止");
    await session.$(barButton(button), { timeout: 30_000 });
  };

  // 1 回目を録音して、議事録を作る
  await pressBarButton(session, "録音を開始");
  await recordOnce("録音を再開");
  await pressBarButton(session, "議事録を作成");
  await session.waitForText(`${sel.minutes}[aria-busy="false"]`, "一回目の内容");
  const firstCount = await utteranceCount(session);

  // 取り消すと、録音を始めない
  await pressBarButton(session, "録音を再開");
  await session.waitForText(dialog, "このノートには議事録があります。同じ会議の続きとして録音しますか？");
  await session.click(dialogButton("取り消す"));
  await session.waitUntil(async () => !(await session.exists(dialog)), { message: "確認が閉じる" });
  await sleep(500);
  assert.equal(await session.exists(timer), false, "取り消したのに録音が始まった");

  // 新しいノートで録音: 新しいノートを作って開き、そこで録音する。元のノートには何も足さない
  await pressBarButton(session, "録音を再開");
  await session.click(dialogButton("新しいノートで録音"));
  await session.waitUntil(async () => (await noteTitles(session)).length === 2, { message: "新しいノートができる" });
  assert.equal(await (await session.$(sel.title)).value(), "", "新しいノートが開いている");
  await recordOnce("録音を再開");
  await openNote(session, "再開のテスト");
  await openTranscript(session);
  await session.waitUntil(async () => (await utteranceCount(session)) === firstCount, { message: "元のノートの文字起こし" });
  assert.equal(await session.exists(".transcript-list .transcript-break"), false, "元のノートに区切りが入った");

  // 続きとして録音: 同じノートで、区切りの後に発話が加わる
  await pressBarButton(session, "録音を再開");
  await session.click(dialogButton("続きとして録音"));
  const heading = await session.waitForText(".transcript-list .transcript-break", "2回目の録音（");
  assert.match(heading, /^2回目の録音（\d{1,2}:\d{2}〜）$/);
  await recordOnce("録音を再開");
  const order = await session.execute(`
    return Array.from(document.querySelectorAll(".transcript-list .utterance-bubble p, .transcript-list .transcript-break"))
      .map((e) => (e.classList.contains("transcript-break") ? "区切り" : "発話"));
  `);
  assert.equal(order.indexOf("区切り"), firstCount, `区切りの位置: ${order.join(",")}`);
  assert.ok(order.length > firstCount + 1, "区切りの後に発話がない");
  assert.deepEqual(await noteTitles(session), ["無題のノート", "再開のテスト"]);

  // 作り直すと、回ごとの節に分けるよう AI に頼む
  ctx.work.answerDialogs({ confirm: true });
  const sent = ctx.server.messages.length;
  await session.click(`xpath://section[@aria-label="議事録"]//button[normalize-space(.)="作り直す"]`);
  await session.waitUntil(() => ctx.server.messages.length > sent, { message: "作り直しの依頼" });
  const content = ctx.server.messages.at(-1).json.messages[0].content;
  assert.ok(content.includes("2 回に分けて録音しています"), content);
  assert.match(content, /--- 2回目の録音（\d{2}:\d{2}〜） ---/);
});
