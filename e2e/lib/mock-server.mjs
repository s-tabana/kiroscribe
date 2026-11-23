// E2E テスト用の模擬サーバー。アプリが外部と通信する先を、1 つの HTTP サーバーで肩代わりする。
//
//   /openai/v1/audio/transcriptions   OpenAI の文字起こし
//   /anthropic/v1/messages            Anthropic の Messages API（SSE のストリーミング）
//   /github-raw/...                   使い方（raw.githubusercontent.com）
//   /github-api/...                   リリース一覧（api.github.com）
//   /update/latest.json・/update/files/...   アプリ内更新の配布元
//
// 受け取ったリクエストはすべて `requests` に記録する（ヘッダと本文を、テストから調べられる）。
// 返す内容は、`openai`・`anthropic`・`github`・`update` のプロパティを書き換えて変える。
// Node の標準ライブラリだけで書いてある。

import http from "node:http";

/** 模擬サーバーが受け付ける API キー（アプリの設定画面には、これを保存する） */
export const OPENAI_KEY = "sk-e2e-openai-0123456789";
export const ANTHROPIC_KEY = "sk-ant-e2e-0123456789";

const REPOSITORY = "s-tabana/kiroscribe";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** multipart/form-data を、項目（文字）とファイルに分ける */
export function parseMultipart(body, contentType) {
  const boundary = /boundary=(?:"([^"]+)"|([^;]+))/i.exec(contentType ?? "");
  if (!boundary) return null;
  const delimiter = Buffer.from(`--${boundary[1] ?? boundary[2]}`);
  const fields = {};
  const files = {};
  let at = body.indexOf(delimiter);
  while (at !== -1) {
    const start = at + delimiter.length;
    if (body.subarray(start, start + 2).toString() === "--") break;
    const next = body.indexOf(delimiter, start);
    if (next === -1) break;
    // 区切りの後ろの CRLF と、次の区切りの前の CRLF を除く
    const part = body.subarray(start + 2, next - 2);
    const headerEnd = part.indexOf("\r\n\r\n");
    if (headerEnd !== -1) {
      const headers = part.subarray(0, headerEnd).toString("utf8");
      const data = part.subarray(headerEnd + 4);
      const name = /name="([^"]*)"/i.exec(headers)?.[1];
      const filename = /filename="([^"]*)"/i.exec(headers)?.[1];
      if (name !== undefined && filename !== undefined) {
        files[name] = {
          filename,
          contentType: /content-type:\s*([^\r\n]+)/i.exec(headers)?.[1]?.trim() ?? null,
          data,
        };
      } else if (name !== undefined) {
        fields[name] = data.toString("utf8");
      }
    }
    at = next;
  }
  return { fields, files };
}

/** 16bit PCM の WAV の概要（文字起こしに送られた音声を確かめるため）。WAV でなければ null */
export function describeWav(data) {
  if (data.length < 44 || data.toString("latin1", 0, 4) !== "RIFF" || data.toString("latin1", 8, 12) !== "WAVE") {
    return null;
  }
  const channels = data.readUInt16LE(22);
  const sampleRate = data.readUInt32LE(24);
  const bitsPerSample = data.readUInt16LE(34);
  const dataBytes = data.readUInt32LE(40);
  return {
    channels,
    sampleRate,
    bitsPerSample,
    seconds: dataBytes / (sampleRate * channels * (bitsPerSample / 8)),
  };
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

function sendJson(res, status, value, headers = {}) {
  const body = Buffer.from(JSON.stringify(value), "utf8");
  res.writeHead(status, { "content-type": "application/json", "content-length": body.length, ...headers });
  res.end(body);
}

function sendText(res, status, text, contentType = "text/plain; charset=utf-8") {
  const body = Buffer.from(text, "utf8");
  res.writeHead(status, { "content-type": contentType, "content-length": body.length });
  res.end(body);
}

// ---- OpenAI ----

/** OpenAI のエラーの本文 */
export function openaiError(message, type = "invalid_request_error", code = null) {
  return { error: { message, type, param: null, code } };
}

const OPENAI_ERRORS = {
  401: openaiError("Incorrect API key provided.", "invalid_request_error", "invalid_api_key"),
  429: openaiError("Rate limit reached for requests", "requests", "rate_limit_exceeded"),
  500: openaiError("The server had an error while processing your request.", "server_error"),
};

// ---- Anthropic ----

/** Anthropic のエラーの本文 */
export function anthropicError(type, message) {
  return { type: "error", error: { type, message } };
}

const ANTHROPIC_ERRORS = {
  401: anthropicError("authentication_error", "invalid x-api-key"),
  429: anthropicError("rate_limit_error", "This request would exceed your rate limit."),
  500: anthropicError("api_error", "Internal server error"),
  529: anthropicError("overloaded_error", "Overloaded"),
};

function sseEvent(res, data) {
  res.write(`event: ${data.type}\ndata: ${JSON.stringify(data)}\n\n`);
}

/**
 * Messages API のストリーミングの応答を書く。
 * reply: { chunks: string[], delayMs?, stopReason?, errorAfter?: { type, message }, before?: Promise, hold?: Promise,
 *          silent?: boolean, tail?: string[] }
 * - errorAfter: chunks を送ったあと、ストリームの途中でエラーのイベントを送る
 * - before: 最初の文字（chunks）を送る前に、この Promise が終わるまで待つ（文字が届く前の表示を確かめるため）
 * - hold: chunks を送ったあと、この Promise が終わるまで応答を進めずに待つ。
 *   途中までの表示や、中止の操作を、時間に頼らずに確かめるために使う
 * - silent: 待っているあいだ（before・hold）、ping も送らない（何も届かない時間を作る）。
 *   指定しなければ、本物の API と同じように ping を送り続ける
 * - tail: hold が終わったあとに送る残りの文字
 */
async function streamMessage(res, model, reply) {
  const { chunks = [], delayMs = 0, stopReason = "end_turn", errorAfter = null, before = null, hold = null, silent = false, tail = [] } = reply;
  /** gate が終わるまで待つ（相手が接続を切ったら、そこでやめる） */
  const waitFor = async (gate) => {
    let released = false;
    gate.then(() => {
      released = true;
    });
    while (!released && !res.destroyed) {
      if (!silent) sseEvent(res, { type: "ping" });
      await sleep(silent ? 20 : 100);
    }
  };
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  sseEvent(res, {
    type: "message_start",
    message: {
      id: "msg_e2e",
      type: "message",
      role: "assistant",
      model,
      content: [],
      stop_reason: null,
      stop_sequence: null,
      usage: { input_tokens: 100, output_tokens: 1 },
    },
  });
  sseEvent(res, { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } });
  sseEvent(res, { type: "ping" });
  if (before) await waitFor(before);
  for (const text of chunks) {
    if (delayMs) await sleep(delayMs);
    if (res.destroyed) return;
    sseEvent(res, { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } });
  }
  if (hold) {
    await waitFor(hold);
    for (const text of tail) {
      if (res.destroyed) return;
      sseEvent(res, { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } });
    }
  }
  if (res.destroyed) return;
  if (errorAfter) {
    sseEvent(res, anthropicError(errorAfter.type, errorAfter.message));
    res.end();
    return;
  }
  sseEvent(res, { type: "content_block_stop", index: 0 });
  sseEvent(res, {
    type: "message_delta",
    delta: { stop_reason: stopReason, stop_sequence: null },
    usage: { output_tokens: chunks.length + tail.length },
  });
  sseEvent(res, { type: "message_stop" });
  res.end();
}

// ---- サーバー本体 ----

export class MockServer {
  constructor() {
    /** 受け取ったリクエスト（古い順） */
    this.requests = [];

    this.openai = {
      /** 受け付ける API キー（`Authorization: Bearer ...`）。違えば 401 を返す */
      apiKey: OPENAI_KEY,
      /** 文字起こしの結果。n は 1 から数えた回数（失敗した回も数える） */
      text: (n) => `模擬の文字起こし ${n}。`,
      /** 次の何回かを失敗させる: { status, body?, times } */
      failure: null,
      /** 応答を返すまでの時間（ミリ秒）。文字起こしを待っているあいだの表示を確かめるために使う */
      delayMs: 0,
      calls: 0,
    };

    this.anthropic = {
      /** 受け付ける API キー（`x-api-key`）。違えば 401 を返す */
      apiKey: ANTHROPIC_KEY,
      /**
       * 応答を決める関数。リクエストの本文（JSON）を受け取り、次のどちらかを返す:
       * - ストリーミングの応答（streamMessage の reply）
       * - { status, body? } … HTTP のエラー
       */
      reply: (body) =>
        body.max_tokens > 20_000
          ? { chunks: ["# 模擬の議事録\n\n", "## 決定事項\n", "- リリースは 12/5\n"] }
          : { chunks: ["模擬の", "回答です。"] },
    };

    this.github = {
      /** 使い方（docs/guide.md）。null なら取得できない（503） */
      guide: "# 模擬の使い方\n\n模擬サーバーから取得した内容です。\n",
      /** リリース一覧（GitHub REST API の形）。null なら取得できない（503） */
      releases: [
        {
          tag_name: "v99.0.0",
          name: "Kiroscribe 99.0.0",
          published_at: "2026-12-01T00:00:00Z",
          body: "- 模擬サーバーのリリースノート",
          html_url: `https://github.com/${REPOSITORY}/releases/tag/v99.0.0`,
          draft: false,
          prerelease: false,
        },
      ],
    };

    this.update = {
      /** latest.json の内容。null なら 404（まだリリースがない） */
      manifest: null,
      /** 配布するファイル（名前 → Buffer） */
      files: new Map(),
    };

    this.server = http.createServer((req, res) => {
      this.#handle(req, res).catch((err) => {
        console.error("[mock-server]", err);
        if (!res.headersSent) sendJson(res, 500, { error: String(err) });
        else res.destroy();
      });
    });
  }

  /** 空いているポートで待ち受けを始める */
  async start() {
    await new Promise((resolve, reject) => {
      this.server.once("error", reject);
      this.server.listen(0, "127.0.0.1", resolve);
    });
    this.url = `http://127.0.0.1:${this.server.address().port}`;
    return this;
  }

  async stop() {
    this.server.closeAllConnections();
    await new Promise((resolve) => this.server.close(resolve));
  }

  /** アプリの設定（config.json）に書く接続先 */
  get endpoints() {
    return {
      openaiBaseUrl: `${this.url}/openai/v1`,
      anthropicBaseUrl: `${this.url}/anthropic/v1`,
      githubRawBaseUrl: `${this.url}/github-raw`,
      githubApiBaseUrl: `${this.url}/github-api`,
      updateEndpoint: `${this.url}/update/latest.json`,
    };
  }

  /** 記録したリクエストのうち、パスが prefix で始まるもの */
  requestsTo(prefix) {
    return this.requests.filter((request) => request.path.startsWith(prefix));
  }

  /** 文字起こしのリクエスト */
  get transcriptions() {
    return this.requestsTo("/openai/v1/audio/transcriptions");
  }

  /** Messages API のリクエスト */
  get messages() {
    return this.requestsTo("/anthropic/v1/messages");
  }

  async #handle(req, res) {
    const url = new URL(req.url, this.url);
    const body = await readBody(req);
    const contentType = req.headers["content-type"] ?? "";
    const record = {
      method: req.method,
      path: url.pathname,
      query: Object.fromEntries(url.searchParams),
      headers: req.headers,
      body,
      json: null,
      multipart: null,
      at: Date.now(),
    };
    if (contentType.includes("application/json")) {
      try {
        record.json = JSON.parse(body.toString("utf8"));
      } catch {
        record.json = null;
      }
    } else if (contentType.includes("multipart/form-data")) {
      record.multipart = parseMultipart(body, contentType);
    }
    this.requests.push(record);

    const route = `${req.method} ${url.pathname}`;
    if (route === "POST /openai/v1/audio/transcriptions") return this.#transcription(record, res);
    if (route === "POST /anthropic/v1/messages") return this.#message(record, res);
    if (route === `GET /github-raw/${REPOSITORY}/main/docs/guide.md`) {
      return this.github.guide === null
        ? sendText(res, 503, "Service Unavailable")
        : sendText(res, 200, this.github.guide);
    }
    if (route === `GET /github-api/repos/${REPOSITORY}/releases`) {
      return this.github.releases === null
        ? sendJson(res, 503, { message: "Service Unavailable" })
        : sendJson(res, 200, this.github.releases);
    }
    if (route === "GET /update/latest.json") {
      return this.update.manifest === null
        ? sendJson(res, 404, { message: "Not Found" })
        : sendJson(res, 200, this.update.manifest);
    }
    if (req.method === "GET" && url.pathname.startsWith("/update/files/")) {
      const file = this.update.files.get(decodeURIComponent(url.pathname.slice("/update/files/".length)));
      if (!file) return sendText(res, 404, "Not Found");
      res.writeHead(200, { "content-type": "application/octet-stream", "content-length": file.length });
      return res.end(file);
    }
    return sendJson(res, 404, { message: `模擬サーバーにない宛先です: ${route}` });
  }

  async #transcription(record, res) {
    const { openai } = this;
    openai.calls += 1;
    const call = openai.calls;
    if (openai.delayMs) await sleep(openai.delayMs);
    if (record.headers.authorization !== `Bearer ${openai.apiKey}`) {
      return sendJson(res, 401, OPENAI_ERRORS[401]);
    }
    if (openai.failure && openai.failure.times > 0) {
      openai.failure.times -= 1;
      const { status, body } = openai.failure;
      return sendJson(res, status, body ?? OPENAI_ERRORS[status] ?? openaiError(`HTTP ${status}`));
    }
    const file = record.multipart?.files.file;
    if (!file || !record.multipart.fields.model) {
      return sendJson(res, 400, openaiError("file と model が必要です"));
    }
    if (!describeWav(file.data)) {
      return sendJson(res, 400, openaiError("Invalid file format."));
    }
    return sendJson(res, 200, { text: openai.text(call, record) });
  }

  async #message(record, res) {
    const { anthropic } = this;
    if (record.headers["x-api-key"] !== anthropic.apiKey) {
      return sendJson(res, 401, ANTHROPIC_ERRORS[401]);
    }
    const body = record.json;
    if (!body || !Array.isArray(body.messages) || !body.max_tokens || !body.model) {
      return sendJson(res, 400, anthropicError("invalid_request_error", "model・max_tokens・messages が必要です"));
    }
    if (!record.headers["anthropic-version"]) {
      return sendJson(res, 400, anthropicError("invalid_request_error", "anthropic-version: header is required"));
    }
    const reply = anthropic.reply(body, record);
    if (reply.status) {
      const fallback = anthropicError("api_error", `HTTP ${reply.status}`);
      return sendJson(res, reply.status, reply.body ?? ANTHROPIC_ERRORS[reply.status] ?? fallback);
    }
    if (!body.stream) {
      return sendJson(res, 400, anthropicError("invalid_request_error", "模擬サーバーは stream: true だけに対応しています"));
    }
    return streamMessage(res, body.model, reply);
  }
}

export async function startMockServer() {
  return new MockServer().start();
}

/** 外から終わらせられる Promise（応答を途中で止めておく hold に使う）: const gate = createGate(); … gate.open(); */
export function createGate() {
  let open;
  const promise = new Promise((resolve) => {
    open = resolve;
  });
  return { promise, open };
}
