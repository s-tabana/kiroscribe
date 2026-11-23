// 起動時の失敗: データベースを開けないときは、理由を知らせて終了する。
// アプリの画面が出る前に終了するので、WebDriver は使わず、アプリを直接起動して確かめる。

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "../lib/app.mjs";
import { noteTitles } from "../lib/ui.mjs";

test("[DAT-007] データベースを開けないときは、理由を知らせるダイアログを出して終了し、空のデータベースで始めない", async (ctx) => {
  // 壊れたデータベース（SQLite のファイルではない内容）を置いておく
  const dbFile = path.join(ctx.work.dataDir, "kiroscribe.db");
  fs.mkdirSync(ctx.work.dataDir, { recursive: true });
  fs.writeFileSync(dbFile, "データベースではない内容");

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

  // 原因を取り除けば、次の起動はふつうに始まる
  fs.rmSync(dbFile);
  const session = await ctx.launch();
  assert.deepEqual(await noteTitles(session), []);
  assert.equal(await ctx.app.isMainVisible(), true);
});
