// E2E テストの土台: 作業フォルダ・模擬サーバー・アプリの起動（tauri-driver + WebDriver）をまとめる。
//
// テスト 1 件ごとに、作業フォルダ（一時フォルダ）と模擬サーバーを 1 つずつ用意する。
// アプリは「E2E テスト用のビルド」（cargo の feature `e2e`）で、環境変数 KIROSCRIBE_E2E_DIR の作業フォルダを見て、
// データの場所・接続先・音声・会議の検出・ダイアログなどを差し替える（crates/core/src/e2e/・src-tauri/src/e2e.rs）。

import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { test as nodeTest } from "node:test";
import { fileURLToPath } from "node:url";
import { ANTHROPIC_KEY, OPENAI_KEY, startMockServer } from "./mock-server.mjs";
import { Session, waitForServer } from "./webdriver.mjs";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const ARTIFACTS_DIR = path.join(ROOT, "e2e", "artifacts");
const IS_WINDOWS = process.platform === "win32";
const EXE = IS_WINDOWS ? ".exe" : "";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** テストするアプリの実行ファイル（既定は、npm run build:e2e で作ったもの） */
export function appBinary() {
  return process.env.KIROSCRIBE_E2E_APP ?? path.join(ROOT, "target", "debug", `kiroscribe${EXE}`);
}

/** tauri-driver の実行ファイル（既定は、cargo install tauri-driver の置き場所） */
export function tauriDriverBinary() {
  return process.env.TAURI_DRIVER ?? path.join(os.homedir(), ".cargo", "bin", `tauri-driver${EXE}`);
}

export function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

/** 起動した tauri-driver とアプリ。テストが途中で打ち切られても残さないよう、このプロセスの終了時に止める */
const spawned = { drivers: new Set(), workspaces: new Set() };
process.on("exit", () => {
  for (const driver of spawned.drivers) driver.kill();
  for (const workspace of spawned.workspaces) {
    const pid = workspace.pid();
    if (pid === null) continue;
    try {
      process.kill(pid, "SIGKILL");
    } catch {
      // 既に終了している
    }
  }
});

function isAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** ファイル名に使えない文字を置き換える */
function safeName(name) {
  return name.replace(/[^\p{L}\p{N}._-]+/gu, "_").slice(0, 80);
}

/**
 * 作業フォルダ。アプリの設定を書き、実行中のアプリとファイルでやり取りする。
 * 中身の意味は crates/core/src/e2e/mod.rs の先頭の説明と同じ。
 */
export class Workspace {
  constructor(dir) {
    this.dir = dir;
    this.dataDir = path.join(dir, "data");
  }

  file(name) {
    return path.join(this.dir, name);
  }

  #read(name) {
    try {
      return fs.readFileSync(this.file(name), "utf8");
    } catch {
      return null;
    }
  }

  /** 起動時に読まれる設定（config.json）を書く */
  configure(config) {
    fs.writeFileSync(this.file("config.json"), JSON.stringify(config, null, 2));
  }

  /** API キーを保存済みの状態にする（設定画面を通さずに用意したいテスト用） */
  seedApiKeys({ openai = OPENAI_KEY, anthropic = ANTHROPIC_KEY } = {}) {
    fs.mkdirSync(this.dataDir, { recursive: true });
    const keys = {};
    if (openai) keys.openai = openai;
    if (anthropic) keys.anthropic = anthropic;
    fs.writeFileSync(path.join(this.dataDir, "secrets.json"), JSON.stringify(keys));
  }

  /** マイクを開けなかったことにする（message が理由の文）。null で元に戻す */
  setMicError(message) {
    if (message) fs.writeFileSync(this.file("mic-error.txt"), message);
    else fs.rmSync(this.file("mic-error.txt"), { force: true });
  }

  /**
   * アプリのデータベースを直接開いて、fn に渡す（アプリを止めているあいだに、状態を作り込むために使う）。
   * Node に組み込みの SQLite（node:sqlite）を使う。
   */
  async withDatabase(fn) {
    const { DatabaseSync } = await import("node:sqlite");
    const db = new DatabaseSync(path.join(this.dataDir, "kiroscribe.db"));
    try {
      return fn(db);
    } finally {
      db.close();
    }
  }

  /** アプリと tauri-driver のログ（起動した回ごとのファイルをつないだもの） */
  logs() {
    return fs
      .readdirSync(this.dir)
      .filter((name) => /^(app-\d+|second-instance)\.log$/.test(name))
      .map((name) => fs.readFileSync(this.file(name), "utf8"))
      .join("\n");
  }

  /** いま会議中として扱う会議アプリ（"googleMeet"・"slackHuddle"・"zoom"・"teams"・"discord"）。null で会議を終える */
  setMeeting(app) {
    if (app) fs.writeFileSync(this.file("meeting.txt"), app);
    else fs.rmSync(this.file("meeting.txt"), { force: true });
  }

  /** 確認ダイアログ・保存ダイアログへの答えを決める（アプリが次にダイアログを出すときから有効） */
  answerDialogs({ confirm = false, savePath = null } = {}) {
    fs.writeFileSync(this.file("dialogs.json"), JSON.stringify({ confirm, savePath }));
  }

  /** アプリが出そうとしたダイアログ（古い順） */
  dialogs() {
    return (this.#read("dialogs.log") ?? "")
      .split("\n")
      .filter(Boolean)
      .map((line) => JSON.parse(line));
  }

  /** アプリがクリップボードへ書いた文字（まだなければ null） */
  clipboard() {
    return this.#read("clipboard.txt");
  }

  /** アプリが外部ブラウザで開こうとした URL */
  openedUrls() {
    return (this.#read("opened-urls.log") ?? "").split("\n").filter(Boolean);
  }

  /** 利用者がウィンドウの「×」を押したのと同じように、ウィンドウを閉じる */
  closeWindow(label) {
    fs.writeFileSync(this.file("close-window.txt"), label);
  }

  /** 起動しているアプリのプロセス ID（まだ起動していなければ null） */
  pid() {
    const text = this.#read("app.pid");
    return text ? Number(text.trim()) : null;
  }

  /** データフォルダの中のファイル（再帰的に）。API キーが平文で残っていないことを調べるために使う */
  dataFiles() {
    const files = [];
    const walk = (dir) => {
      for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        const full = path.join(dir, entry.name);
        if (entry.isDirectory()) walk(full);
        else if (entry.isFile()) files.push(full);
      }
    };
    if (fs.existsSync(this.dataDir)) walk(this.dataDir);
    return files;
  }
}

/** 起動したアプリ 1 つ（tauri-driver のプロセスと、WebDriver のセッション） */
export class App {
  constructor(workspace, driver, session, logFile) {
    this.workspace = workspace;
    this.driver = driver;
    this.session = session;
    this.logFile = logFile;
    this.mainHandle = null;
  }

  /** tauri-driver を起動し、その下でアプリを起動する。環境変数は tauri-driver → ネイティブのドライバー → アプリへ引き継がれる */
  static async launch(workspace, { index = 1, waitForMain = true, args: appArgs = [] } = {}) {
    const [port, nativePort] = [await freePort(), await freePort()];
    const logFile = workspace.file(`app-${index}.log`);
    const log = fs.openSync(logFile, "a");
    const args = ["--port", String(port), "--native-port", String(nativePort)];
    if (process.env.KIROSCRIBE_E2E_NATIVE_DRIVER) {
      args.push("--native-driver", process.env.KIROSCRIBE_E2E_NATIVE_DRIVER);
    }
    const driver = spawn(tauriDriverBinary(), args, {
      env: appEnvironment(workspace),
      stdio: ["ignore", log, log],
      windowsHide: true,
    });
    fs.closeSync(log);
    spawned.drivers.add(driver);
    spawned.workspaces.add(workspace);
    const failed = new Promise((_, reject) => {
      driver.once("error", (err) => reject(new Error(`tauri-driver を起動できません（${tauriDriverBinary()}）: ${err.message}`)));
      driver.once("exit", (code) => reject(new Error(`tauri-driver が終了しました（終了コード ${code}）。ログ: ${logFile}`)));
    });
    failed.catch(() => {}); // 正常に止めたときの「終了しました」は無視する

    const baseUrl = `http://127.0.0.1:${port}`;
    let session;
    try {
      await Promise.race([waitForServer(baseUrl), failed]);
      session = await Promise.race([Session.create(baseUrl, { application: appBinary(), args: appArgs }), failed]);
    } catch (err) {
      driver.kill();
      throw new Error(
        `アプリを起動できませんでした: ${err.message}\n` +
          `（Windows では、msedgedriver と WebView2 のバージョンが合っていないと、ここで止まることがあります）\n` +
          tail(logFile),
      );
    }
    const app = new App(workspace, driver, session, logFile);
    if (waitForMain) {
      app.mainHandle = await session.currentWindow();
      await session.$(".sidebar", { timeout: 30_000 });
    }
    return app;
  }

  /** メインウィンドウを操作の対象にする */
  async main() {
    await this.session.switchToWindow(this.mainHandle);
    return this.session;
  }

  /** 会議検出のポップアップが出るまで待って、操作の対象にする */
  async popup({ timeout = 15_000 } = {}) {
    const { session } = this;
    await session.waitUntil(
      async () => {
        for (const handle of await session.windowHandles()) {
          if (handle === this.mainHandle) continue;
          await session.switchToWindow(handle);
          if ((await session.url()).includes("#/popup")) return true;
        }
        await session.switchToWindow(this.mainHandle);
        return false;
      },
      { timeout, message: "会議検出のポップアップが出る" },
    );
    return session;
  }

  /** いま開いているポップアップのウィンドウのハンドル（操作の対象はメインウィンドウに戻す） */
  async popupHandles() {
    const { session } = this;
    const found = [];
    for (const handle of await session.windowHandles()) {
      if (handle === this.mainHandle) continue;
      try {
        await session.switchToWindow(handle);
        if ((await session.url()).includes("#/popup")) found.push(handle);
      } catch {
        // 調べているあいだに閉じた
      }
    }
    await session.switchToWindow(this.mainHandle);
    return found;
  }

  /** ポップアップのウィンドウがあるか（操作の対象はメインウィンドウに戻す） */
  async hasPopup() {
    const { session } = this;
    let found = false;
    for (const handle of await session.windowHandles()) {
      if (handle === this.mainHandle) continue;
      try {
        await session.switchToWindow(handle);
        if ((await session.url()).includes("#/popup")) found = true;
      } catch {
        // 調べているあいだに閉じた
      }
    }
    await session.switchToWindow(this.mainHandle);
    return found;
  }

  /** メインウィンドウが表示されているか（閉じてトレイにいる間は false） */
  async isMainVisible() {
    const session = await this.main();
    return session.executeAsync(`() => window.__TAURI_INTERNALS__.invoke("plugin:window|is_visible", { label: "main" })`);
  }

  /** アプリのプロセスが動いているか */
  isRunning() {
    const pid = this.workspace.pid();
    return pid !== null && isAlive(pid);
  }

  /** アプリを終了する */
  async stop() {
    const pid = this.workspace.pid();
    await this.session.quit().catch(() => {});
    this.driver.removeAllListeners("exit");
    this.driver.kill();
    // トレイに常駐するアプリなので、ドライバーが止まっても残ることがある。残っていたら止める
    if (pid !== null) {
      for (let i = 0; i < 50 && isAlive(pid); i++) await sleep(100);
      if (isAlive(pid)) {
        try {
          process.kill(pid, "SIGKILL");
        } catch {
          // 既に終了している
        }
        for (let i = 0; i < 50 && isAlive(pid); i++) await sleep(100);
      }
    }
    fs.rmSync(this.workspace.file("app.pid"), { force: true });
    spawned.drivers.delete(this.driver);
    // Windows では、ウェブビューのプロセスが少し遅れて終わる。同じデータのまますぐ起動し直すと、
    // 前のプロセスがデータフォルダをつかんでいて起動に失敗することがあるので、少し待つ
    if (IS_WINDOWS) await sleep(1_500);
  }
}

export function appEnvironment(workspace) {
  const env = { ...process.env, KIROSCRIBE_E2E_DIR: workspace.dir };
  // 模擬サーバー（127.0.0.1）への通信を、プロキシに回さない
  const noProxy = [process.env.NO_PROXY ?? process.env.no_proxy, "127.0.0.1", "localhost"].filter(Boolean).join(",");
  env.NO_PROXY = noProxy;
  env.no_proxy = noProxy;
  return env;
}

function tail(file, lines = 30) {
  try {
    const text = fs.readFileSync(file, "utf8").trimEnd();
    return text ? `--- ${path.basename(file)} の末尾 ---\n${text.split("\n").slice(-lines).join("\n")}` : "";
  } catch {
    return "";
  }
}

/** アプリの既定の設定: 接続先はすべて模擬サーバー、音声は合成音、待ち時間は短く */
function defaultConfig(server) {
  return {
    ...server.endpoints,
    audio: { mic: "speech", system: "speech" },
    timing: {
      meetingPollMs: 100,
      meetingStartConfirmMs: 300,
      meetingEndGraceMs: 2_000,
      updateFirstCheckMs: 500,
      retryBaseDelayMs: 20,
    },
  };
}

/** テスト 1 件ぶんの道具 */
export class Context {
  constructor(name, server, workspace) {
    this.name = name;
    /** 模擬サーバー（e2e/lib/mock-server.mjs） */
    this.server = server;
    /** 作業フォルダ */
    this.work = workspace;
    /** 起動中のアプリ */
    this.app = null;
    this.launches = 0;
  }

  static async create(name, { config = {}, apiKeys = true } = {}) {
    const server = await startMockServer();
    const dir = fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), "kiroscribe-e2e-"));
    const workspace = new Workspace(dir);
    const base = defaultConfig(server);
    workspace.configure({
      ...base,
      ...config,
      audio: { ...base.audio, ...config.audio },
      timing: { ...base.timing, ...config.timing },
    });
    if (apiKeys) workspace.seedApiKeys(apiKeys === true ? {} : apiKeys);
    return new Context(name, server, workspace);
  }

  /**
   * アプリを起動して、メインウィンドウの WebDriver のセッションを返す。
   * options: { args?: アプリに渡す引数（例: ["--autostart"]）, waitForMain?: false でメイン画面の表示を待たない }
   */
  async launch(options) {
    this.launches += 1;
    this.app = await App.launch(this.work, { index: this.launches, ...options });
    return this.app.session;
  }

  /** アプリを終了して、同じデータのまま起動し直す。options は launch と同じ（例: { args: ["--autostart"] }） */
  async relaunch(options) {
    await this.stop();
    return this.launch(options);
  }

  /** アプリを終了する（データはそのまま残る） */
  async stop() {
    await this.app.stop();
    this.app = null;
  }

  /**
   * 同じ作業フォルダ（＝同じデータ）で、アプリをもう 1 つ直接起動する。終了するまで待って、終了コードを返す。
   * 多重起動の防止が働けば、2 つ目はすぐに終了する。
   */
  async launchSecondInstance({ timeout = 30_000, args = [] } = {}) {
    const log = fs.openSync(this.work.file("second-instance.log"), "a");
    const child = spawn(appBinary(), args, { env: appEnvironment(this.work), stdio: ["ignore", log, log] });
    fs.closeSync(log);
    const exited = new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("exit", (code, signal) => resolve({ code, signal }));
    });
    const timer = sleep(timeout).then(() => "timeout");
    const result = await Promise.race([exited, timer]);
    if (result === "timeout") {
      child.kill("SIGKILL");
      throw new Error(`2 つ目のアプリが ${timeout} ミリ秒たっても終了しません（多重起動しています）`);
    }
    return result;
  }

  /** 失敗したときに、調べるための材料を e2e/artifacts/<テスト名>/ に残す */
  async saveArtifacts(error) {
    const dir = path.join(ARTIFACTS_DIR, safeName(this.name));
    fs.rmSync(dir, { recursive: true, force: true });
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, "error.txt"), `${error?.stack ?? error}\n`);

    const session = this.app?.session;
    if (session) {
      try {
        const handles = await session.windowHandles();
        for (const [index, handle] of handles.entries()) {
          await session.switchToWindow(handle);
          const name = handle === this.app.mainHandle ? "main" : `window-${index}`;
          fs.writeFileSync(path.join(dir, `${name}.png`), await session.screenshot());
          fs.writeFileSync(path.join(dir, `${name}.html`), await session.source());
        }
      } catch (err) {
        fs.writeFileSync(path.join(dir, "screenshot-error.txt"), `${err?.stack ?? err}\n`);
      }
    }
    // アプリと tauri-driver のログ、アプリとやり取りしたファイル（データフォルダは除く）
    for (const entry of fs.readdirSync(this.work.dir, { withFileTypes: true })) {
      if (entry.isFile()) fs.copyFileSync(this.work.file(entry.name), path.join(dir, entry.name));
    }
    const requests = this.server.requests.map((request) => ({
      at: new Date(request.at).toISOString(),
      method: request.method,
      path: request.path,
      query: request.query,
      headers: request.headers,
      json: request.json,
      multipart: request.multipart && {
        fields: request.multipart.fields,
        files: Object.fromEntries(
          Object.entries(request.multipart.files).map(([name, file]) => [
            name,
            { filename: file.filename, contentType: file.contentType, bytes: file.data.length },
          ]),
        ),
      },
      bodyBytes: request.body.length,
    }));
    fs.writeFileSync(path.join(dir, "mock-server-requests.json"), JSON.stringify(requests, null, 2));
    return dir;
  }

  async dispose() {
    if (this.app) await this.app.stop().catch(() => {});
    await this.server.stop();
    spawned.workspaces.delete(this.work);
    try {
      fs.rmSync(this.work.dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    } catch (err) {
      // ウェブビューのプロセスが少し遅れて終わり、ファイルをつかんでいることがある（一時フォルダなので残っても害はない）
      console.warn(`作業フォルダを消せませんでした（${this.work.dir}）: ${err.message}`);
    }
  }
}

/**
 * E2E テストを 1 件定義する（node:test の test と同じ形で書ける）。
 * options: { config?: アプリの設定の上書き, apiKeys?: false | { openai, anthropic }, timeout?: ミリ秒 }
 * fn にはテスト 1 件ぶんの道具（Context）が渡る。失敗したら、スクリーンショットとログを e2e/artifacts/ に残す。
 *
 * 仕様の ID を付けるときは、テストの名前の先頭に角かっこで書く（例: 「[NOTE-001] 起動するとメイン画面が出て…」）。
 */
export function test(name, options, fn) {
  if (typeof options === "function") [options, fn] = [{}, options];
  const { timeout = 180_000, ...contextOptions } = options;
  nodeTest(name, { timeout }, async () => {
    const context = await Context.create(name, contextOptions);
    try {
      await fn(context);
    } catch (error) {
      const dir = await context.saveArtifacts(error).catch((err) => `（保存できませんでした: ${err.message}）`);
      error.message += `\n調べるための材料: ${dir}`;
      throw error;
    } finally {
      await context.dispose();
    }
  });
}
