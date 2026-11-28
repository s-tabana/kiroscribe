// 会議の自動検出を、製品と同じ仕組み（Windows のオーディオのセッションと、ウィンドウのタイトル）で確かめる
// （Windows だけ。CI では仮想のオーディオケーブルを使う。e2e/lib/audio-device.mjs）。
//
// ほかのテストは meeting.txt で会議中かどうかを決めるが、ここでは検出を模擬せず（config の realMeetingDetection）、
// - デスクトップの会議アプリ: マイクを開いたまま待つだけのプログラム（crates/core/examples/hold_mic.rs）を
//   「Zoom.exe」「Discord.exe」の名前で動かす
// - ブラウザの会議: Edge で「Meet - …」の題のページを開き、そのページがマイクを使う
// ことで、実際に Windows のオーディオのセッションを作り、アプリがそれを読んで録音を始め・止めることを確かめる。
// 実際のサービス（Zoom などそのもの）がいつマイクを使うかは、ここでは確かめない（リリース前のチェックリスト）。
// 環境変数 KIROSCRIBE_E2E_REAL_AUDIO=1 のときだけ走る。

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { ROOT, test } from "../lib/app.mjs";
import { noteTitles, sel } from "../lib/ui.mjs";

const REAL_AUDIO = process.platform === "win32" && process.env.KIROSCRIBE_E2E_REAL_AUDIO === "1";
const skip = !REAL_AUDIO && "仮想のオーディオケーブルを入れた Windows でだけ走る（KIROSCRIBE_E2E_REAL_AUDIO=1）";
const HOLD_MIC = path.join(ROOT, "target", "debug", "examples", "hold_mic.exe");
const MEETING_PROBE = path.join(ROOT, "target", "debug", "examples", "meeting_probe.exe");
const EDGE = [
  path.join(process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)", "Microsoft", "Edge", "Application", "msedge.exe"),
  path.join(process.env.ProgramFiles ?? "C:\\Program Files", "Microsoft", "Edge", "Application", "msedge.exe"),
].find((file) => fs.existsSync(file));
/** 会議が見えなくなってから録音を止めるまでの猶予（このテストでの値。製品では 20 秒） */
const GRACE_MS = 3_000;
const options = {
  skip,
  timeout: 240_000,
  config: { audio: { mic: null, system: null }, realMeetingDetection: true, timing: { meetingEndGraceMs: GRACE_MS } },
};

/** 子プロセスを、その子孫ごと止める（終わるまで待つ） */
function killTree(child) {
  if (child.exitCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    const taskkill = spawn("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
    taskkill.once("exit", resolve);
    taskkill.once("error", resolve);
  });
}

/** マイクを使い、会議の窓を出すだけのプログラムを、会議アプリの実行ファイルの名前で動かす */
function startFakeApp(ctx, exeName, windowArgs) {
  assert.ok(fs.existsSync(HOLD_MIC), `${HOLD_MIC} がありません（cargo build -p kiroscribe-core --example hold_mic）`);
  const dir = path.join(ctx.work.dir, "apps", path.parse(exeName).name);
  fs.mkdirSync(dir, { recursive: true });
  const exe = path.join(dir, exeName);
  fs.copyFileSync(HOLD_MIC, exe);
  const log = fs.openSync(ctx.work.file(`${exeName}.log`), "a");
  const child = spawn(exe, ["120", ...windowArgs], { stdio: ["ignore", log, log], windowsHide: true });
  fs.closeSync(log);
  return child;
}

/** 検出に失敗した時点の判定・窓・部品・セッション（確認用の道具の表示）と、テスト用プロセスの出力。 */
function detectionDiagnostics(ctx) {
  const probe = spawnSync(MEETING_PROBE, ["--once", "--titles"], { encoding: "utf8", windowsHide: true, timeout: 20_000 });
  const probeLog = [probe.stdout, probe.stderr, probe.error?.message].filter(Boolean).join("\n");
  fs.writeFileSync(ctx.work.file("meeting-probe.log"), probeLog);
  const logs = fs
    .readdirSync(ctx.work.dir)
    .filter((name) => name.endsWith(".exe.log"))
    .map((name) => `--- ${name} ---\n${fs.readFileSync(ctx.work.file(name), "utf8")}`);
  return `--- meeting_probe ---\n${probeLog}\n${logs.join("\n")}`;
}

/** 会議が始まって、ポップアップとノートができ、録音が始まるまで待つ。できたノートのタイトルを返す */
async function waitForMeetingStart(ctx, session, label) {
  await ctx.app.popup({ timeout: 60_000 });
  const started = await session.waitForText(sel.popup, "自動で録音・文字起こしを始めました");
  assert.ok(started.includes(label), started);
  await ctx.app.main();
  await session.waitUntil(async () => (await noteTitles(session)).some((t) => t.startsWith(`${label} `)), {
    message: `「${label} …」のノートができる`,
  });
  await session.$(`${sel.noteItems} [aria-label="録音中"]`);
  return (await noteTitles(session)).find((t) => t.startsWith(`${label} `));
}

/** 会議が終わって、自動で始めた録音が止まるまで待つ */
async function waitForMeetingEnd(ctx, session) {
  try {
    await ctx.app.popup({ timeout: 60_000 });
    await session.waitForText(sel.popup, "録音を停止しました", { timeout: 60_000 });
    await ctx.app.main();
    await session.waitUntil(async () => !(await session.exists(`${sel.noteItems} [aria-label="録音中"]`)), {
      message: "録音中の印が消える",
    });
  } catch (error) {
    error.message += `\n${detectionDiagnostics(ctx)}`;
    throw error;
  }
}

for (const [exeName, label, windowArgs] of [
  ["Zoom.exe", "Zoom", ["--window", "ZPContentViewWndClass"]],
  ["Discord.exe", "Discord", ["--window", "HoldMicDiscordWindow", "--button", "Disconnect"]],
]) {
  test(`[MTG-001][MTG-020][MTG-024] デスクトップの会議アプリ（${exeName}）が会議の窓を出すと録音を始め、閉じると止める`, options, async (ctx) => {
    const session = await ctx.launch();
    // メインウィンドウを閉じて（トレイに残して）おく
    ctx.work.closeWindow("main");
    await session.waitUntil(async () => !(await ctx.app.isMainVisible()), { message: "メインウィンドウが隠れる" });

    const app = startFakeApp(ctx, exeName, windowArgs);
    try {
      await waitForMeetingStart(ctx, session, label);
      // 自分自身（録音中の Kiroscribe）がマイクを使っていても、会議の判定には入れない（会議アプリが止まれば終わる）
    } catch (error) {
      error.message += `\n${detectionDiagnostics(ctx)}`;
      throw error;
    } finally {
      await killTree(app);
    }
    await waitForMeetingEnd(ctx, session);
  });
}

test("[MTG-002][MTG-020][MTG-024] ブラウザ（Edge）で「Meet - …」の会議のページを開くと録音を始め、閉じると止める", { ...options, skip: options.skip || (!EDGE && "Edge がない") }, async (ctx) => {
  // 退出ボタンがあり、マイクを使うページ（localhost は安全な場所として扱われ、マイクを使える）
  const page = `<!doctype html><meta charset="utf-8"><title>Meet - abc-defg-hij</title>
<p id="state">マイクを準備しています</p>
<button type="button">Leave call</button>
<script>
navigator.mediaDevices.getUserMedia({ audio: true })
  .then((stream) => { window.stream = stream; document.getElementById("state").textContent = "マイクを使っています"; })
  .catch((err) => { document.title = "error " + err.name; });
</script>`;
  const server = http.createServer((req, res) => {
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    res.end(page);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${server.address().port}/`;

  const session = await ctx.launch();
  ctx.work.closeWindow("main");
  await session.waitUntil(async () => !(await ctx.app.isMainVisible()), { message: "メインウィンドウが隠れる" });

  // 使い捨てのプロファイルで、マイクの許可を自動で与えて開く（本物のマイク＝仮想のケーブルを使う）
  const profile = path.join(ctx.work.dir, "edge-profile");
  const edge = spawn(
    EDGE,
    [
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-features=msEdgeWelcomePage",
      "--use-fake-ui-for-media-stream",
      `--app=${url}`,
    ],
    { stdio: "ignore" },
  );
  try {
    await waitForMeetingStart(ctx, session, "Google Meet");
  } catch (error) {
    error.message += `\n${detectionDiagnostics(ctx)}`;
    throw error;
  } finally {
    await killTree(edge);
    server.close();
  }
  await waitForMeetingEnd(ctx, session);
});
