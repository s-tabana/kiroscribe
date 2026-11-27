// 起動時の失敗: データベースを開けないときは、理由を知らせて終了する。
// アプリの画面が出る前に終了するので、WebDriver は使わず、アプリを直接起動して確かめる。

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { appBinary, appEnvironment, test } from "../lib/app.mjs";
import { noteTitles } from "../lib/ui.mjs";
import { pressButton, visibleWindows } from "../lib/uia.mjs";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** 壊れたデータベース（SQLite のファイルではない内容）を置く */
function breakDatabase(ctx) {
  const dbFile = path.join(ctx.work.dataDir, "kiroscribe.db");
  fs.mkdirSync(ctx.work.dataDir, { recursive: true });
  fs.writeFileSync(dbFile, "データベースではない内容");
  return dbFile;
}

test("[DAT-007][LOG-003] データベースを開けないときは、理由を知らせるダイアログを出して終了し、空のデータベースで始めない", async (ctx) => {
  const dbFile = breakDatabase(ctx);

  const exited = await ctx.launchSecondInstance({ timeout: 60_000 });
  assert.notEqual(exited.code, 0, "起動に失敗したのに、正常終了している");

  // 理由を知らせるダイアログ（E2E 用のビルドでは、出そうとしたダイアログが記録される）
  const dialogs = ctx.work.dialogs();
  assert.equal(dialogs.length, 1, JSON.stringify(dialogs));
  assert.equal(dialogs[0].type, "message");
  assert.equal(dialogs[0].kind, "error");
  assert.equal(dialogs[0].title, "Kiroscribe");
  assert.match(dialogs[0].message, /^Kiroscribe を起動できませんでした。/);
  assert.ok(dialogs[0].message.includes("データベース"), dialogs[0].message);
  assert.ok(dialogs[0].message.includes(dbFile), `どのファイルかが分からない: ${dialogs[0].message}`);
  assert.ok(dialogs[0].message.includes("データは変更していません"), dialogs[0].message);
  // 壊れたファイルを、空のデータベースで置き換えていない（中身を調べたり、直したりできるように残す）
  assert.equal(fs.readFileSync(dbFile, "utf8"), "データベースではない内容");
  // デバッグ用のログに、起動したことと、起動できなかった理由を書いている
  const logDir = path.join(ctx.work.dataDir, "logs");
  const log = fs
    .readdirSync(logDir)
    .filter((name) => /^kiroscribe-\d{8}\.log$/.test(name))
    .map((name) => fs.readFileSync(path.join(logDir, name), "utf8"))
    .join("");
  const events = log
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
  assert.ok(events.some((e) => e.event === "app.started" && e.fields.appVersion), log);
  const failed = events.find((e) => e.event === "app.start_failed");
  assert.ok(failed && failed.level === "error" && failed.fields.reason.includes("データベース"), log);

  // 原因を取り除けば、次の起動はふつうに始まる
  fs.rmSync(dbFile);
  const session = await ctx.launch();
  assert.deepEqual(await noteTitles(session), []);
  assert.equal(await ctx.app.isMainVisible(), true);
});

test(
  "[DAT-007] Windows では、起動できないときに OS のダイアログが出て、メインウィンドウは出さず、OK を押すと終了する",
  {
    skip: process.platform !== "win32" && "Windows の画面部品（UI オートメーション）で確かめるテスト",
    config: { nativeStartupDialog: true },
  },
  async (ctx) => {
    const dbFile = breakDatabase(ctx);
    const log = fs.openSync(ctx.work.file("app-1.log"), "a");
    const child = spawn(appBinary(), [], { env: appEnvironment(ctx.work), stdio: ["ignore", log, log] });
    fs.closeSync(log);
    const exited = new Promise((resolve) => child.once("exit", (code) => resolve(code)));
    try {
      // 利用者が見るとおりに読む: 題名「Kiroscribe」のダイアログだけが出ている
      let windows = [];
      const deadline = Date.now() + 60_000;
      // 文とボタンがそろうまで待つ（ダイアログは、出始めてから部品がそろうまでに少しかかる）
      // OK は、ダイアログの作りによって、ボタンではなくほかの種類の部品として見える（タスク ダイアログでは Pane）
      const parts = (w) => [...w.buttons, ...w.others.map((other) => other.slice(other.indexOf(":") + 1))];
      const ready = (w) => w.texts.some((text) => text.includes("Kiroscribe を起動できませんでした")) && parts(w).includes("OK");
      while (!windows.some(ready)) {
        if (Date.now() >= deadline || child.exitCode !== null) {
          const appLog = fs.readFileSync(ctx.work.file("app-1.log"), "utf8").split("\n").slice(-30).join("\n");
          assert.fail(`起動の失敗のダイアログが出ません（終了コード ${child.exitCode}）: ${JSON.stringify(windows)}\n--- アプリのログ ---\n${appLog}`);
        }
        await sleep(500);
        windows = visibleWindows(child.pid);
      }
      assert.equal(windows.length, 1, `ダイアログのほかにウィンドウが出ている: ${JSON.stringify(windows)}`);
      const [dialog] = windows;
      assert.equal(dialog.name, "Kiroscribe");
      const text = dialog.texts.join("\n");
      assert.ok(text.includes("データベース"), text);
      assert.ok(text.includes("データは変更していません"), text);
      assert.ok(parts(dialog).includes("OK"), `OK のボタンがない: ${JSON.stringify(dialog)}`);

      // 閉じるまでは待っている（読まないうちに消えない）
      await sleep(1_500);
      assert.equal(child.exitCode, null, "ダイアログを閉じる前に終了した");

      // OK を押すと終了する（トレイにも残らない）
      assert.ok(pressButton(child.pid, "OK"), "OK のボタンを押せません");
      const code = await Promise.race([exited, sleep(20_000).then(() => "timeout")]);
      assert.equal(code, 1, "OK を押しても終了しない、または終了コードが 1 でない");
    } finally {
      if (child.exitCode === null) child.kill();
    }
    assert.equal(fs.readFileSync(dbFile, "utf8"), "データベースではない内容");
  },
);
