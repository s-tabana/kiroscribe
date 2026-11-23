// 模擬サーバーそのもののテスト（アプリは起動しない）: node --test e2e/lib/

import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { ANTHROPIC_KEY, OPENAI_KEY, createGate, describeWav, parseMultipart, startMockServer } from "./mock-server.mjs";

/** 0.5 秒・16kHz・モノラルの WAV（中身は無音） */
function wav(seconds = 0.5, sampleRate = 16_000) {
  const dataBytes = Math.round(seconds * sampleRate) * 2;
  const buffer = Buffer.alloc(44 + dataBytes);
  buffer.write("RIFF", 0, "latin1");
  buffer.writeUInt32LE(36 + dataBytes, 4);
  buffer.write("WAVEfmt ", 8, "latin1");
  buffer.writeUInt32LE(16, 16);
  buffer.writeUInt16LE(1, 20);
  buffer.writeUInt16LE(1, 22);
  buffer.writeUInt32LE(sampleRate, 24);
  buffer.writeUInt32LE(sampleRate * 2, 28);
  buffer.writeUInt16LE(2, 32);
  buffer.writeUInt16LE(16, 34);
  buffer.write("data", 36, "latin1");
  buffer.writeUInt32LE(dataBytes, 40);
  return buffer;
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

let server;
before(async () => {
  server = await startMockServer();
});
after(() => server.stop());

function transcribe({ key = OPENAI_KEY, file = wav(), fields = { model: "gpt-4o-transcribe", language: "ja" } } = {}) {
  const form = new FormData();
  if (file) form.set("file", new Blob([file], { type: "audio/wav" }), "audio.wav");
  for (const [name, value] of Object.entries(fields)) form.set(name, value);
  return fetch(`${server.endpoints.openaiBaseUrl}/audio/transcriptions`, {
    method: "POST",
    headers: key ? { authorization: `Bearer ${key}` } : {},
    body: form,
  });
}

function message(body, { key = ANTHROPIC_KEY, version = "2026-10-21" } = {}) {
  return fetch(`${server.endpoints.anthropicBaseUrl}/messages`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...(key ? { "x-api-key": key } : {}),
      ...(version ? { "anthropic-version": version } : {}),
    },
    body: JSON.stringify(body),
  });
}

const messageBody = {
  model: "claude-opus-5-5",
  max_tokens: 64_000,
  stream: true,
  messages: [{ role: "user", content: "まとめて" }],
};

/** SSE の本文を、イベント（data の JSON）の並びにする */
function sseEvents(text) {
  return text
    .split("\n\n")
    .filter((block) => block.trim())
    .map((block) => {
      const lines = block.split("\n");
      const event = lines.find((line) => line.startsWith("event: ")).slice(7);
      const data = JSON.parse(lines.find((line) => line.startsWith("data: ")).slice(6));
      assert.equal(event, data.type, "event の行と data の type が同じ");
      return data;
    });
}

test("文字起こし: multipart を受け取り、文字と記録を返す", async () => {
  const before = server.transcriptions.length;
  const response = await transcribe({ fields: { model: "gpt-4o-transcribe", language: "ja", prompt: "直前の発話" } });
  assert.equal(response.status, 200);
  assert.match((await response.json()).text, /^模擬の文字起こし \d+。$/);

  const recorded = server.transcriptions.at(-1);
  assert.equal(server.transcriptions.length, before + 1);
  assert.equal(recorded.headers.authorization, `Bearer ${OPENAI_KEY}`);
  assert.deepEqual(recorded.multipart.fields, { model: "gpt-4o-transcribe", language: "ja", prompt: "直前の発話" });
  const file = recorded.multipart.files.file;
  assert.equal(file.filename, "audio.wav");
  assert.equal(file.contentType, "audio/wav");
  assert.deepEqual(describeWav(file.data), { channels: 1, sampleRate: 16_000, bitsPerSample: 16, seconds: 0.5 });
});

test("文字起こし: キーが違えば 401、指定した回数だけ 429・500 を返す", async () => {
  for (const key of ["sk-wrong", null]) {
    const response = await transcribe({ key });
    assert.equal(response.status, 401);
    assert.equal((await response.json()).error.code, "invalid_api_key");
  }

  server.openai.failure = { status: 429, times: 2 };
  assert.equal((await transcribe()).status, 429);
  const second = await transcribe();
  assert.equal(second.status, 429);
  assert.equal((await second.json()).error.code, "rate_limit_exceeded");
  assert.equal((await transcribe()).status, 200, "回数を使い切ったら成功に戻る");

  server.openai.failure = { status: 500, times: 1 };
  const failed = await transcribe();
  assert.equal(failed.status, 500);
  assert.equal((await failed.json()).error.type, "server_error");
});

test("文字起こし: 音声でないファイルや、項目の不足は 400", async () => {
  assert.equal((await transcribe({ file: Buffer.from("not audio") })).status, 400);
  assert.equal((await transcribe({ fields: {} })).status, 400);
  assert.equal((await transcribe({ file: null })).status, 400);
});

test("Messages: SSE で少しずつ返す", async () => {
  server.anthropic.reply = () => ({ chunks: ["## 決定", "事項\n", "- 12/5"], delayMs: 5 });
  const response = await message(messageBody);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/event-stream");
  const events = sseEvents(await response.text());

  assert.deepEqual(
    events.map((event) => event.type),
    [
      "message_start",
      "content_block_start",
      "ping",
      "content_block_delta",
      "content_block_delta",
      "content_block_delta",
      "content_block_stop",
      "message_delta",
      "message_stop",
    ],
  );
  assert.equal(events[0].message.model, "claude-opus-5-5");
  const text = events.filter((event) => event.type === "content_block_delta").map((event) => event.delta.text);
  assert.deepEqual(text, ["## 決定", "事項\n", "- 12/5"]);
  assert.ok(text.every((_, i) => events[3 + i].delta.type === "text_delta"));
  assert.equal(events.at(-2).delta.stop_reason, "end_turn");

  const recorded = server.messages.at(-1);
  assert.equal(recorded.headers["x-api-key"], ANTHROPIC_KEY);
  assert.equal(recorded.headers["anthropic-version"], "2026-10-21");
  assert.deepEqual(recorded.json.messages, messageBody.messages);
});

test("Messages: 応答は、リクエストの内容を見て決められる", async () => {
  server.anthropic.reply = (body) => ({ chunks: [`max_tokens=${body.max_tokens}`] });
  const events = sseEvents(await (await message({ ...messageBody, max_tokens: 16_000 })).text());
  assert.equal(events.find((event) => event.type === "content_block_delta").delta.text, "max_tokens=16000");
});

test("Messages: refusal と、ストリームの途中のエラー", async () => {
  server.anthropic.reply = () => ({ chunks: [], stopReason: "refusal" });
  const refused = sseEvents(await (await message(messageBody)).text());
  assert.equal(refused.find((event) => event.type === "message_delta").delta.stop_reason, "refusal");

  server.anthropic.reply = () => ({
    chunks: ["途中まで"],
    errorAfter: { type: "overloaded_error", message: "Overloaded" },
  });
  const broken = sseEvents(await (await message(messageBody)).text());
  assert.deepEqual(broken.at(-1), { type: "error", error: { type: "overloaded_error", message: "Overloaded" } });
  assert.equal(broken.some((event) => event.type === "message_stop"), false, "正常終了のイベントは送らない");
});

test("Messages: 応答を途中で止めておき、合図のあとに残りを返せる", async () => {
  const gate = createGate();
  server.anthropic.reply = () => ({ chunks: ["書きかけ"], hold: gate.promise, tail: ["の続き"] });
  const response = await message(messageBody);
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let received = "";
  while (!received.includes("書きかけ")) {
    received += decoder.decode((await reader.read()).value, { stream: true });
  }
  assert.equal(received.includes("message_stop"), false, "止めているあいだは終わらない");
  assert.equal(received.includes("の続き"), false, "止めているあいだは残りを送らない");
  gate.open();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    received += decoder.decode(value, { stream: true });
  }
  const events = sseEvents(received);
  const text = events.filter((event) => event.type === "content_block_delta").map((event) => event.delta.text);
  assert.deepEqual(text, ["書きかけ", "の続き"]);
  assert.ok(events.some((event) => event.type === "ping"));
  assert.equal(events.at(-1).type, "message_stop");
});

test("Messages: 最初の文字の前でも止めておける。silent なら、止めているあいだ何も送らない", async () => {
  const before = createGate();
  const hold = createGate();
  server.anthropic.reply = () => ({ before: before.promise, chunks: ["前半"], hold: hold.promise, silent: true, tail: ["後半"] });
  const response = await message(messageBody);
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let received = "";
  /** ms ミリ秒のあいだに届いた分を読む（読みかけの read() は 1 つだけにする） */
  let pending = null;
  const pump = async (ms) => {
    const deadline = Date.now() + ms;
    while (Date.now() < deadline) {
      pending ??= reader.read();
      const next = await Promise.race([pending, sleep(deadline - Date.now()).then(() => null)]);
      if (next === null) return;
      pending = null;
      if (next.done) return;
      received += decoder.decode(next.value, { stream: true });
    }
  };
  await pump(300);
  assert.ok(received.includes("message_start") && !received.includes("前半"), received);
  const pings = received.split("event: ping").length - 1;
  before.open();
  await pump(300);
  assert.ok(received.includes("前半") && !received.includes("後半"), received);
  assert.equal(received.split("event: ping").length - 1, pings, "silent なのに、止めているあいだに ping を送っている");
  hold.open();
  await pump(1_000);
  assert.ok(received.includes("後半") && received.includes("message_stop"), received);
});

test("文字起こし: 応答を遅らせられる", async () => {
  server.openai.delayMs = 300;
  try {
    const started = Date.now();
    const response = await transcribe();
    assert.equal(response.status, 200);
    assert.ok(Date.now() - started >= 280, "遅らせていない");
  } finally {
    server.openai.delayMs = 0;
  }
});

test("Messages: キーが違えば 401、HTTP のエラーも返せる", async () => {
  server.anthropic.reply = () => ({ chunks: ["OK"] });
  for (const key of ["sk-ant-wrong", null]) {
    const response = await message(messageBody, { key });
    assert.equal(response.status, 401);
    assert.deepEqual(await response.json(), {
      type: "error",
      error: { type: "authentication_error", message: "invalid x-api-key" },
    });
  }
  assert.equal((await message(messageBody, { version: null })).status, 400, "anthropic-version がない");
  assert.equal((await message({ model: "claude-opus-5-5" })).status, 400, "messages がない");

  server.anthropic.reply = () => ({ status: 529 });
  const overloaded = await message(messageBody);
  assert.equal(overloaded.status, 529);
  assert.equal((await overloaded.json()).error.type, "overloaded_error");

  server.anthropic.reply = () => ({ status: 400, body: { type: "error", error: { type: "invalid_request_error", message: "独自の本文" } } });
  assert.equal((await (await message(messageBody)).json()).error.message, "独自の本文");
});

test("GitHub: 使い方とリリース一覧。止めると 503", async () => {
  const guideUrl = `${server.endpoints.githubRawBaseUrl}/s-tabana/kiroscribe/main/docs/guide.md`;
  const releasesUrl = `${server.endpoints.githubApiBaseUrl}/repos/s-tabana/kiroscribe/releases?per_page=20`;

  const guide = await fetch(guideUrl, { headers: { accept: "text/plain" } });
  assert.equal(guide.status, 200);
  assert.match(await guide.text(), /模擬サーバーから取得した内容です/);

  const releases = await fetch(releasesUrl, { headers: { accept: "application/vnd.github+json" } });
  assert.equal(releases.status, 200);
  const list = await releases.json();
  assert.equal(list[0].tag_name, "v99.0.0");
  assert.equal(server.requestsTo("/github-api/").at(-1).query.per_page, "20");
  assert.equal(server.requestsTo("/github-api/").at(-1).headers.accept, "application/vnd.github+json");

  server.github.guide = null;
  server.github.releases = null;
  assert.equal((await fetch(guideUrl)).status, 503);
  assert.equal((await fetch(releasesUrl)).status, 503);
});

test("更新: latest.json とファイル。リリースがなければ 404", async () => {
  const latest = server.endpoints.updateEndpoint;
  assert.equal((await fetch(latest)).status, 404);

  server.update.manifest = { version: "99.0.0", platforms: {} };
  server.update.files.set("Kiroscribe_99.0.0.bin", Buffer.from([1, 2, 3]));
  assert.deepEqual(await (await fetch(latest)).json(), { version: "99.0.0", platforms: {} });
  const file = await fetch(`${server.url}/update/files/Kiroscribe_99.0.0.bin`);
  assert.equal(file.headers.get("content-length"), "3");
  assert.deepEqual([...new Uint8Array(await file.arrayBuffer())], [1, 2, 3]);
  assert.equal((await fetch(`${server.url}/update/files/missing.bin`)).status, 404);
});

test("知らない宛先は 404 で、記録には残る", async () => {
  const response = await fetch(`${server.url}/nowhere`);
  assert.equal(response.status, 404);
  assert.equal(server.requests.at(-1).path, "/nowhere");
});

test("multipart の解析: 引用符つきの境界と、空のファイル", () => {
  const body = Buffer.from(
    '--b1\r\nContent-Disposition: form-data; name="model"\r\n\r\nwhisper-1\r\n' +
      '--b1\r\nContent-Disposition: form-data; name="file"; filename="a.wav"\r\nContent-Type: audio/wav\r\n\r\n\r\n' +
      "--b1--\r\n",
  );
  const parsed = parseMultipart(body, 'multipart/form-data; boundary="b1"');
  assert.deepEqual(parsed.fields, { model: "whisper-1" });
  assert.equal(parsed.files.file.data.length, 0);
  assert.equal(parseMultipart(body, "application/json"), null);
  assert.equal(describeWav(Buffer.from("short")), null);
});
