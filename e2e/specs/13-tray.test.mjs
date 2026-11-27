// タスクトレイ: アイコン・ツールチップ・左クリック・右クリックのメニューとその項目（Windows だけ）。
// 通知領域はエクスプローラー（タスクバー）の画面なので、WebDriver は届かない。UI オートメーションで読み、
// クリックは利用者と同じく本物のマウスの操作で行う（e2e/lib/uia.mjs）。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";
import { closeSettings, openSettings, settingsCheck } from "../lib/ui.mjs";
import { chooseMenuItem, clickTrayIcon, describeTopWindows, minimizeWindow, openMenu, trayIcons } from "../lib/uia.mjs";

const TOOLTIP = "Kiroscribe（会議の自動検出）";
const OPEN = "Kiroscribe を開く";
const STOP = "録音を停止";
const AUTO_RECORD = "会議を検出したら自動で録音";
const QUIT = "Kiroscribe を終了";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const skip = process.platform !== "win32" && "Windows のタスクバー（UI オートメーション）で確かめるテスト";

function isAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function until(fn, { timeout = 15_000, message }) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await fn();
    if (value) return value;
    if (Date.now() >= deadline) assert.fail(`${timeout / 1000} 秒待っても、次のようになりません: ${message}`);
    await sleep(300);
  }
}

/** 右クリックのメニューが出るまで待って、項目を返す。出なければ、画面のウィンドウの一覧を付けて失敗にする */
async function menuItems(pid) {
  try {
    return await until(() => openMenu(pid), { timeout: 10_000, message: "右クリックのメニューが出る" });
  } catch (error) {
    error.message += `\n--- 画面のウィンドウ ---\n${describeTopWindows()}`;
    throw error;
  }
}

const isMinimized = (session) =>
  session.executeAsync(`() => window.__TAURI_INTERNALS__.invoke("plugin:window|is_minimized", { label: "main" })`);

test(
  "[TRY-001][TRY-002][TRY-003][TRY-004][TRY-005][TRY-006] タスクトレイのアイコンから、ウィンドウを出す・自動録音を切り替える・終了するができる",
  { skip, timeout: 300_000 },
  async (ctx) => {
    const session = await ctx.launch();
    const pid = ctx.work.pid();

    // ウィンドウを × で閉じても、通知領域にアイコン（ツールチップ「Kiroscribe（会議の自動検出）」）が残る
    ctx.work.closeWindow("main");
    await session.waitUntil(async () => !(await ctx.app.isMainVisible()), { message: "メインウィンドウが隠れる" });
    const tray = trayIcons(TOOLTIP);
    assert.equal(tray.count, 1, `通知領域にアイコンが見つかりません。通知領域にあるもの:\n${tray.tray.join("\n")}`);

    // 左クリックで、メインウィンドウが出る
    assert.ok(clickTrayIcon(TOOLTIP, "left"), "アイコンをクリックできません");
    await session.waitUntil(() => ctx.app.isMainVisible(), { message: "左クリックでメインウィンドウが出る" });

    // 最小化してから左クリックしても、元に戻って出る
    assert.ok(minimizeWindow(pid, "Kiroscribe"), "メインウィンドウを最小化できません");
    await session.waitUntil(() => isMinimized(session), { message: "最小化される" });
    assert.ok(clickTrayIcon(TOOLTIP, "left"));
    await session.waitUntil(async () => !(await isMinimized(session)), { message: "左クリックで、最小化していたウィンドウが元に戻る" });

    // 閉じているときにもう一度起動しても、ウィンドウが出るだけで、アイコンは 1 つのまま
    ctx.work.closeWindow("main");
    await session.waitUntil(async () => !(await ctx.app.isMainVisible()), { message: "メインウィンドウが隠れる" });
    await ctx.launchSecondInstance();
    await session.waitUntil(() => ctx.app.isMainVisible(), { message: "もう一度起動すると、メインウィンドウが出る" });
    assert.equal(trayIcons(TOOLTIP).count, 1, "もう一度起動したら、アイコンが 2 つになった");

    // 右クリックで、メニューが出る（「会議を検出したら自動で録音」はチェック付き）
    assert.ok(clickTrayIcon(TOOLTIP, "right"));
    let items = await menuItems(pid);
    assert.deepEqual(
      items.map((item) => item.name).filter(Boolean),
      [OPEN, STOP, AUTO_RECORD, QUIT],
      JSON.stringify(items),
    );
    const checkedAtFirst = items.find((item) => item.name === AUTO_RECORD).checked;
    if (checkedAtFirst !== null) assert.equal(checkedAtFirst, true, "最初からチェックが外れている");

    // 「会議を検出したら自動で録音」を選ぶと、設定の「会議の自動検出」がオフになって保存される
    assert.ok(chooseMenuItem(AUTO_RECORD), "メニューの項目を選べません");
    await sleep(500);
    await openSettings(session);
    const check = await session.$(settingsCheck("会議を検出したら自動で録音・文字起こしする"));
    await session.waitUntil(async () => !(await check.isSelected()), { message: "設定の「会議の自動検出」がオフになる" });
    await closeSettings(session);
    // メニューのチェックも外れている
    assert.ok(clickTrayIcon(TOOLTIP, "right"));
    items = await menuItems(pid);
    const checkedAfter = items.find((item) => item.name === AUTO_RECORD).checked;
    if (checkedAfter !== null) assert.equal(checkedAfter, false, "メニューのチェックが外れていない");

    // 「Kiroscribe を終了」で終了し、通知領域のアイコンも消える
    assert.ok(chooseMenuItem(QUIT));
    await until(() => !isAlive(pid), { timeout: 30_000, message: "アプリが終了する" });
    await until(() => trayIcons(TOOLTIP).count === 0, { timeout: 15_000, message: "通知領域のアイコンが消える" });
  },
);
