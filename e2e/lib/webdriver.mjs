// WebDriver（W3C）の最小限のクライアント。tauri-driver に HTTP で話しかける。
// 使う命令が少ないので、外部のライブラリには頼らず fetch だけで書いてある。
// 仕様: https://www.w3.org/TR/webdriver2/

const ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf";
const REQUEST_TIMEOUT_MS = 60_000;

/** キーボードの特殊キー（WebDriver の仕様で決まっている文字コード） */
export const Key = {
  Null: "",
  Backspace: "",
  Enter: "",
  Shift: "",
  Control: "",
  Escape: "",
};

export class WebDriverError extends Error {
  constructor(command, value) {
    super(`${command}: ${value?.error ?? "unknown error"}: ${value?.message ?? ""}`);
    this.name = "WebDriverError";
    this.code = value?.error;
  }
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** CSS セレクターか、`xpath:` で始まる XPath を、WebDriver の探し方に直す */
function locator(selector) {
  return selector.startsWith("xpath:")
    ? { using: "xpath", value: selector.slice("xpath:".length) }
    : { using: "css selector", value: selector };
}

/** 文字を XPath の文字列リテラルにする（' と " の両方を含んでいてもよい） */
function xpathLiteral(text) {
  if (!text.includes('"')) return `"${text}"`;
  if (!text.includes("'")) return `'${text}'`;
  return `concat(${text.split('"').map((part) => `"${part}"`).join(`, '"', `)})`;
}

/** 表示されている文字で要素を探すセレクター。例: byText("button", "保存") */
export function byText(tag, text, { exact = true } = {}) {
  const literal = xpathLiteral(text);
  return exact
    ? `xpath://${tag}[normalize-space(.)=${literal}]`
    : `xpath://${tag}[contains(normalize-space(.), ${literal})]`;
}

export class Session {
  /** @param {string} baseUrl tauri-driver の URL  @param {string} sessionId */
  constructor(baseUrl, sessionId, capabilities) {
    this.baseUrl = baseUrl;
    this.sessionId = sessionId;
    this.capabilities = capabilities;
    /** 待ちの既定の上限（ミリ秒） */
    this.timeout = 15_000;
  }

  /** アプリを起動してセッションを作る。application はアプリの実行ファイルのパス */
  static async create(baseUrl, { application, args = [] }) {
    const value = await request(baseUrl, "POST", "/session", {
      capabilities: { alwaysMatch: { "tauri:options": { application, args } } },
    });
    return new Session(baseUrl, value.sessionId, value.capabilities);
  }

  command(method, path, body) {
    return request(this.baseUrl, method, `/session/${this.sessionId}${path}`, body);
  }

  /** セッションを終える（アプリも終了する） */
  async quit() {
    await this.command("DELETE", "");
  }

  // ---- 待つ ----

  /**
   * check が値（false・null・undefined 以外）を返すまで待って、その値を返す。
   * 途中のエラー（要素がまだない等）は、時間切れまで無視する。
   */
  async waitUntil(check, { timeout = this.timeout, interval = 100, message = "条件" } = {}) {
    const deadline = Date.now() + timeout;
    let lastError;
    for (;;) {
      try {
        const value = await check();
        if (value !== false && value !== null && value !== undefined) return value;
        lastError = undefined;
      } catch (err) {
        lastError = err;
      }
      if (Date.now() >= deadline) {
        const detail = lastError ? `（最後のエラー: ${lastError.message}）` : "";
        throw new Error(`${timeout} ミリ秒待っても満たされませんでした: ${message}${detail}`);
      }
      await sleep(interval);
    }
  }

  // ---- 要素 ----

  async findAll(selector) {
    const found = await this.command("POST", "/elements", locator(selector));
    return found.map((ref) => new Element(this, ref[ELEMENT_KEY], selector));
  }

  /** いま存在する要素（なければ null） */
  async find(selector) {
    return (await this.findAll(selector))[0] ?? null;
  }

  async exists(selector) {
    return (await this.findAll(selector)).length > 0;
  }

  /** 要素が現れて表示されるまで待つ */
  async $(selector, options = {}) {
    return this.waitUntil(
      async () => {
        const element = await this.find(selector);
        return element && (await element.isDisplayed()) ? element : null;
      },
      { message: `${selector} が表示される`, ...options },
    );
  }

  /** 要素がなくなる（または見えなくなる）まで待つ */
  async waitGone(selector, options = {}) {
    await this.waitUntil(
      async () => {
        const element = await this.find(selector);
        return !element || !(await element.isDisplayed().catch(() => false));
      },
      { message: `${selector} が消える`, ...options },
    );
  }

  async click(selector, options) {
    await (await this.$(selector, options)).click();
  }

  /**
   * 押すとそのウィンドウが閉じるボタンを押す。
   * 押した結果ウィンドウが閉じると、ドライバーは「ウィンドウがない」というエラーを返すことがあるので、それは成功として扱う。
   * このあとは、別のウィンドウに切り替えてから操作する。
   */
  async clickClosing(selector, options) {
    const element = await this.$(selector, options);
    try {
      await element.click();
    } catch (err) {
      if (!(err instanceof WebDriverError) || err.code !== "no such window") throw err;
    }
  }

  async text(selector, options) {
    return (await this.$(selector, options)).text();
  }

  /** 要素の文字が期待どおりになるまで待つ（expected は文字列＝部分一致、または正規表現） */
  async waitForText(selector, expected, options = {}) {
    let last = "";
    const matches = (text) => (typeof expected === "string" ? text.includes(expected) : expected.test(text));
    try {
      return await this.waitUntil(
        async () => {
          const element = await this.find(selector);
          if (!element) return null;
          last = await element.text();
          return matches(last) ? last : null;
        },
        { message: `${selector} の文字が ${expected} になる`, ...options },
      );
    } catch (err) {
      throw new Error(`${err.message}\n最後に見えていた文字: ${JSON.stringify(last)}`);
    }
  }

  // ---- ページ ----

  /** ページの中でスクリプトを実行する（同期。戻り値は JSON にできるもの） */
  execute(script, ...args) {
    return this.command("POST", "/execute/sync", { script, args });
  }

  /** ページの中で Promise を返す関数を実行して、結果を待つ。失敗したら例外にする */
  async executeAsync(fn, ...args) {
    const script = `
      const done = arguments[arguments.length - 1];
      Promise.resolve()
        .then(() => (${fn}).apply(null, Array.prototype.slice.call(arguments, 0, -1)))
        .then((value) => done({ ok: true, value }), (error) => done({ ok: false, error: String(error?.message ?? error) }));
    `;
    const result = await this.command("POST", "/execute/async", { script, args });
    if (!result.ok) throw new Error(result.error);
    return result.value;
  }

  url() {
    return this.command("GET", "/url");
  }

  title() {
    return this.command("GET", "/title");
  }

  source() {
    return this.command("GET", "/source");
  }

  /** スクリーンショット（PNG） */
  async screenshot() {
    return Buffer.from(await this.command("GET", "/screenshot"), "base64");
  }

  // ---- ウィンドウ ----

  windowHandles() {
    return this.command("GET", "/window/handles");
  }

  currentWindow() {
    return this.command("GET", "/window");
  }

  switchToWindow(handle) {
    return this.command("POST", "/window", { handle });
  }
}

export class Element {
  constructor(session, id, selector) {
    this.session = session;
    this.id = id;
    this.selector = selector;
  }

  command(method, path, body) {
    return this.session.command(method, `/element/${this.id}${path}`, body);
  }

  click() {
    return this.command("POST", "/click", {});
  }

  /** キーボードから入力する（改行は Enter キーとして送る。"\n" のままだと入力されないドライバーがある） */
  type(text) {
    return this.command("POST", "/value", { text: text.replaceAll("\n", Key.Enter) });
  }

  /** 入力欄を空にしてから入力する（全選択して上書きする。利用者の操作と同じく、入力のイベントが起きる） */
  async fill(text) {
    await this.command("POST", "/value", { text: `${Key.Control}a${Key.Null}${Key.Backspace}` });
    if (text) await this.type(text);
  }

  /**
   * 画面に見えている文字（innerText）。
   * WebDriver の「要素の文字を取る」命令は、ドライバーによって入れ子の要素の文字を落とすことがあるので使わない。
   */
  text() {
    return this.session.execute("return arguments[0].innerText", { [ELEMENT_KEY]: this.id });
  }

  attribute(name) {
    return this.command("GET", `/attribute/${encodeURIComponent(name)}`);
  }

  property(name) {
    return this.command("GET", `/property/${encodeURIComponent(name)}`);
  }

  /** 入力欄の値 */
  value() {
    return this.property("value");
  }

  isDisplayed() {
    return this.command("GET", "/displayed");
  }

  isEnabled() {
    return this.command("GET", "/enabled");
  }

  isSelected() {
    return this.command("GET", "/selected");
  }
}

async function request(baseUrl, method, path, body) {
  const response = await fetch(baseUrl + path, {
    method,
    headers: { "content-type": "application/json; charset=utf-8" },
    body: body === undefined ? undefined : JSON.stringify(body),
    // ドライバーが応答しなくなっても、テスト全体が止まらないようにする
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  const text = await response.text();
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new WebDriverError(`${method} ${path}`, { error: `HTTP ${response.status}`, message: text.slice(0, 300) });
  }
  const value = parsed.value;
  if (!response.ok || (value && typeof value === "object" && "error" in value && "message" in value)) {
    throw new WebDriverError(`${method} ${path}`, value);
  }
  return value;
}

/** WebDriver のサーバー（tauri-driver）が応答するようになるまで待つ */
export async function waitForServer(baseUrl, { timeout = 15_000 } = {}) {
  const deadline = Date.now() + timeout;
  for (;;) {
    try {
      await fetch(`${baseUrl}/status`);
      return;
    } catch (err) {
      if (Date.now() >= deadline) throw new Error(`${baseUrl} が応答しません: ${err.message}`);
      await sleep(100);
    }
  }
}
