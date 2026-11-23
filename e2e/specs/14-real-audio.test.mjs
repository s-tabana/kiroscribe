// 本物の音声デバイスでの録音（Windows だけ。CI では仮想のオーディオケーブルを使う。e2e/lib/audio-device.mjs）。
//
// ほかのテストは模擬の音声（アプリの中で作る合成音）を使うが、ここでは模擬の音声を切り、
// アプリが OS の音声デバイス（WASAPI）から、マイクと PC から流れる音声を取り込むところから確かめる。
// 文字起こしの API は模擬サーバーのまま（届いた音声が無音でないことを、サーバーの側で調べる）。
// 環境変数 KIROSCRIBE_E2E_REAL_AUDIO=1 のときだけ走る（仮想のオーディオケーブルを入れた CI のジョブ）。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";
import { CABLE, audioEndpoints, playSpeech, setAudioServiceRunning, setDesktopMicAccess, setDeviceEnabled, wavRms } from "../lib/audio-device.mjs";
import { closeSettings, createNote, dismissToast, openSettings, pressBarButton, saveSettings, sel, settingsCheck, toastText, utterances } from "../lib/ui.mjs";

const REAL_AUDIO = process.platform === "win32" && process.env.KIROSCRIBE_E2E_REAL_AUDIO === "1";
const skip = !REAL_AUDIO && "仮想のオーディオケーブルを入れた Windows でだけ走る（KIROSCRIBE_E2E_REAL_AUDIO=1）";
/** 模擬の音声を使わない（アプリが本物のデバイスを開く） */
const options = { skip, timeout: 240_000, config: { audio: { mic: null, system: null } } };
const timer = `${sel.recordingBar} [role="timer"]`;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** 設定画面のマイクの欄の選択肢（表示されている文字） */
async function micOptions(session) {
  return session.execute(`
    const label = Array.from(document.querySelectorAll("label")).find((l) => l.querySelector("span")?.textContent === "マイク");
    return Array.from(label.querySelectorAll("option")).map((o) => o.textContent);
  `);
}

/** 文字起こしの API に届いた音声のうち、無音でないものの数（話者は問わない） */
function audibleUploads(ctx) {
  return ctx.server.transcriptions.filter((request) => wavRms(request.multipart.files.file.data) > 0.01).length;
}

async function speakersShown(session) {
  return new Set((await utterances(session)).map((u) => u.speaker));
}

test("[SET-006][REC-001][REC-009] 本物の音声デバイスから、マイクと PC の音声を取り込んで文字起こしに送り、一時停止のあいだは送らない", options, async (ctx) => {
  console.log(`[e2e] 音声デバイス: ${JSON.stringify(audioEndpoints())}`);
  const session = await ctx.launch();

  // 設定のマイクの欄に、既定のマイクとして仮想のケーブルの名前が出る
  await openSettings(session);
  await session.waitUntil(async () => (await micOptions(session)).length > 1, { message: "マイクの一覧が出る" });
  const choices = await micOptions(session);
  assert.match(choices[0], /^既定のマイク（.*CABLE Output.*）$/, JSON.stringify(choices));
  assert.ok(choices.slice(1).some((name) => name.includes("CABLE Output")), JSON.stringify(choices));
  await closeSettings(session);

  // 録音を始めて、PC で音を鳴らす → マイク（自分）と PC の音声（相手）の両方から届き、文字起こしに送られる
  await createNote(session, { title: "本物の音声デバイス" });
  await pressBarButton(session, "録音を開始");
  await session.waitForText(timer, "録音中");
  const playing = playSpeech(ctx.work.dir, 12);
  // 鳴らしているあいだ、自分と相手の入力の大きさのバーが動く
  await session.waitUntil(
    () =>
      session.execute(
        "return ['me', 'them'].every((s) => document.querySelectorAll(`.level-${s} .level-bars i.on`).length > 0)",
      ),
    { timeout: 15_000, message: "自分と相手の入力の大きさのバーが動く" },
  );
  await playing;
  await session.waitUntil(async () => (await speakersShown(session)).size === 2, {
    timeout: 30_000,
    message: "自分と相手の発話が表示される",
  });
  assert.ok(audibleUploads(ctx) >= 2, `無音でない音声が送られていない: ${ctx.server.transcriptions.length} 件中 ${audibleUploads(ctx)} 件`);

  // 一時停止のあいだに鳴らした音は、送らない
  await pressBarButton(session, "一時停止");
  await session.waitForText(timer, "一時停止中");
  await sleep(3_000); // 一時停止の前に区切った分が、送り終わるのを待つ
  const beforePause = ctx.server.transcriptions.length;
  await playSpeech(ctx.work.dir, 8);
  await sleep(2_000);
  assert.equal(ctx.server.transcriptions.length, beforePause, "一時停止のあいだの音声を送った");

  // 再開すると、また送る
  await pressBarButton(session, "再開");
  await session.waitForText(timer, "録音中");
  await playSpeech(ctx.work.dir, 8);
  await session.waitUntil(() => ctx.server.transcriptions.length > beforePause, { timeout: 30_000, message: "再開のあとの音声が送られる" });

  await pressBarButton(session, "停止");
  await session.$(`xpath://*[@aria-label="録音"]//button[normalize-space(.)="録音を再開"]`, { timeout: 30_000 });
});

test("[REC-017] 設定で「相手の声も取り込む」をオフにすると、本物のデバイスでも、マイクだけを取り込む", options, async (ctx) => {
  const session = await ctx.launch();
  await openSettings(session);
  const check = await session.$(settingsCheck("相手の声（PC から流れる音声）も取り込む"));
  if (await check.isSelected()) await check.click();
  await saveSettings(session);
  await closeSettings(session);

  await createNote(session, { title: "マイクだけ" });
  await pressBarButton(session, "録音を開始");
  await session.waitForText(timer, "録音中");
  await playSpeech(ctx.work.dir, 10);
  await session.waitUntil(async () => (await speakersShown(session)).has("自分"), { timeout: 30_000, message: "自分の発話が出る" });
  await sleep(2_000);
  assert.deepEqual([...(await speakersShown(session))], ["自分"], "相手の声を取り込んでいる");
  await session.waitForText("body", "相手の声は取り込んでいません（マイクのみ）");
  await pressBarButton(session, "停止");
});

test("[REC-019][SET-006] 選んだマイクが見つからないときは、既定のマイクで録音し、そのことを知らせる", options, async (ctx) => {
  // いちど起動してデータベースを作り、いまはないデバイスを選んであることにする（USB のマイクを抜いたあと）
  await ctx.launch();
  await ctx.stop();
  await ctx.work.withDatabase((db) => {
    const row = db.prepare("SELECT value FROM settings WHERE key = 'app'").get();
    const settings = row ? JSON.parse(row.value) : {};
    settings.inputDeviceId = "{0.0.1.00000000}.{00000000-0000-0000-0000-000000000000}";
    db.prepare("INSERT OR REPLACE INTO settings (key, value) VALUES ('app', ?)").run(JSON.stringify(settings));
  });

  const session = await ctx.launch();
  await openSettings(session);
  await session.waitUntil(async () => (await micOptions(session)).includes("（見つからないデバイス）"), {
    message: "マイクの欄が「（見つからないデバイス）」になる",
  });
  await closeSettings(session);

  await createNote(session, { title: "見つからないマイク" });
  await pressBarButton(session, "録音を開始");
  await toastText(session, "選択したマイクが見つからないため、既定のマイクで録音します");
  await dismissToast(session);
  await session.waitForText(timer, "録音中");
  await playSpeech(ctx.work.dir, 8);
  await session.waitUntil(async () => (await speakersShown(session)).has("自分"), {
    timeout: 30_000,
    message: "既定のマイクで録音できている",
  });
  await pressBarButton(session, "停止");
});

test("[REC-021] 録音中に音声デバイスで問題が起きても、エラーを知らせて落ちず、停止できる", options, async (ctx) => {
  const session = await ctx.launch();
  await createNote(session, { title: "デバイスの問題" });
  await pressBarButton(session, "録音を開始");
  await session.waitForText(timer, "録音中");
  try {
    // 録音中のデバイスは、OS が無効にさせてくれない（使用中）。代わりに Windows の音声サービスを止めて、
    // 開いているデバイスを使えなくする（USB のマイクを抜いたときと同じく、アプリにはデバイスのエラーが届く）
    setAudioServiceRunning(false);
    await toastText(session, "の音声デバイスでエラーが発生しました", { timeout: 30_000 });
    assert.equal(ctx.app.isRunning(), true, "デバイスがなくなって、アプリが落ちた");
    await dismissToast(session);
    await pressBarButton(session, "停止");
    await session.$(`xpath://*[@aria-label="録音"]//button[normalize-space(.)="録音を再開"]`, { timeout: 30_000 });
  } finally {
    setAudioServiceRunning(true);
    await sleep(3_000);
  }
});

test("[REC-020] 使えるマイクがないときは、録音を始めずに知らせる", options, async (ctx) => {
  const session = await ctx.launch();
  await createNote(session, { title: "マイクがない" });
  try {
    setDeviceEnabled(CABLE, false);
    await sleep(3_000);
    const inputs = audioEndpoints().filter((d) => d.status === "OK" && /microphone|マイク|input|入力/i.test(d.name));
    assert.deepEqual(inputs, [], `仮想のケーブルのほかに、使える入力デバイスがある: ${JSON.stringify(inputs)}`);
    await pressBarButton(session, "録音を開始");
    await toastText(session, "マイクが見つかりません。接続と Windows のプライバシー設定（マイクへのアクセス）を確認してください");
    assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false, "マイクがないのに、録音中になった");
  } finally {
    setDeviceEnabled(CABLE, true);
    await sleep(3_000);
  }
});

test("[REC-022] Windows のプライバシー設定でマイクが許可されていないときは、録音を始めず、直す場所を知らせる", options, async (ctx) => {
  const session = await ctx.launch();
  await createNote(session, { title: "マイクの許可がない" });
  try {
    setDesktopMicAccess("Deny");
    await sleep(1_000);
    await pressBarButton(session, "録音を開始");
    const shown = await toastText(session, "マイクを使う許可がありません");
    assert.ok(shown.includes("プライバシーとセキュリティ"), shown);
    assert.ok(!/access is denied|os error/i.test(shown), `Windows の技術的な文が出ている: ${shown}`);
    assert.equal(await session.exists(`${sel.noteItems} [aria-label="録音中"]`), false, "許可がないのに、録音中になった");
  } finally {
    setDesktopMicAccess("Allow");
  }
});
