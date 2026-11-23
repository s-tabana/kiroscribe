// 手動の録音: 模擬の音声が「自分」「相手」として文字起こしされ、一時停止・停止できる。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";
import { describeWav } from "../lib/mock-server.mjs";
import { createNote, dismissToast, noteTitles, pressBarButton, sel, toastText, utterances } from "../lib/ui.mjs";

const barButton = (text) => `xpath://*[@aria-label="録音"]//button[normalize-space(.)="${text}"]`;

test("録音を開始すると文字起こしが「自分」「相手」で表示され、停止できる。録音中のノートは削除できない", async (ctx) => {
  const session = await ctx.launch();
  await createNote(session, { title: "録音のテスト" });

  await pressBarButton(session, "録音を開始");
  await session.waitForText(`${sel.recordingBar} [role="timer"]`, "録音中");
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

  // 停止（残りの文字起こしを待ってから終わる）
  await pressBarButton(session, "停止");
  await session.$(barButton("録音を再開"), { timeout: 30_000 });
  assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false);
  const shown = await utterances(session);
  assert.ok(shown.length >= 2, "停止後も文字起こしが残っている");

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
    assert.ok(wav.seconds >= 1 && wav.seconds <= 26, `発話の長さ: ${wav.seconds} 秒`);
  }
  // 2 回目以降は、直前の発話を文脈として渡す
  assert.ok(ctx.server.transcriptions.some((request) => request.multipart.fields.prompt?.includes("模擬の文字起こし")));

  // 停止後は削除できる
  await session.click('button[aria-label="ノートを削除"]');
  await session.waitUntil(async () => (await noteTitles(session)).length === 0, { message: "ノートが消える" });
});

test(
  "相手の声を取り込めないときは、マイクだけで録音を続けて、そのことを知らせる",
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

test("文字起こしの API がキーを拒否したら、文字起こしを止めて、キーの確認を促す", async (ctx) => {
  ctx.server.openai.apiKey = "sk-別のキー";
  const session = await ctx.launch();
  await createNote(session, { title: "キーの誤り" });
  await pressBarButton(session, "録音を開始");
  const message = await toastText(session, "文字起こしを停止しました。API キーを確認してください", { timeout: 30_000 });
  assert.match(message, /OpenAI の API キーが正しくないか、権限がありません（HTTP 401/);
  // キーに関する知らせには、設定画面への近道が付く
  await session.click(`${sel.toast} button.button`);
  await session.$(sel.settingsDialog);
});

test("文字起こしの API の一時的なエラー（429・500）は、再試行して成功すれば表に出さない", async (ctx) => {
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
