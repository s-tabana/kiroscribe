// ヘルプ: 使い方とリリースノートをオンライン（模擬サーバー）から取得して表示する。
// 取得できないときは、前回取得した内容か、アプリに同梱の内容を表示する。

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { ROOT, test } from "../lib/app.mjs";
import { sel } from "../lib/ui.mjs";

const panel = `${sel.helpDialog} [role="tabpanel"]`;
const notice = `${sel.helpDialog} [role="note"]`;
const tab = (label) => `xpath://div[@role="dialog"][@aria-label="ヘルプ"]//button[@role="tab"][normalize-space(.)="${label}"]`;

async function openHelp(session) {
  await session.click('button[aria-label="ヘルプ"]');
  await session.$(sel.helpDialog);
}

async function closeHelp(session) {
  await session.click(`${sel.helpDialog} button[aria-label="閉じる"]`);
  await session.waitGone(sel.helpDialog);
}

/** 同梱の使い方（docs/guide.md）の最初の見出し */
function bundledGuideHeading() {
  const guide = fs.readFileSync(path.join(ROOT, "docs", "guide.md"), "utf8");
  return /^#\s+(.+)$/m.exec(guide)[1].trim();
}

test("ヘルプに、オンラインの使い方とリリースノートが表示される。リンクは外部ブラウザで開く", async (ctx) => {
  ctx.server.github.guide = "# 模擬の使い方\n\n模擬サーバーから取得した内容です。\n\n[公式サイト](https://example.com/kiroscribe)\n";
  const session = await ctx.launch();

  await openHelp(session);
  await session.waitForText(panel, "模擬サーバーから取得した内容です。");
  assert.equal(await session.exists(notice), false, "オンラインの内容なら、出どころの注記は出さない");

  // 本文のリンクは、アプリの画面を遷移させずに外部ブラウザで開く
  await session.click(`${panel} a`);
  await session.waitUntil(() => ctx.work.openedUrls().length === 1, { message: "リンクを外部ブラウザで開く" });
  assert.deepEqual(ctx.work.openedUrls(), ["https://example.com/kiroscribe"]);
  assert.match(await session.url(), /^(tauri:\/\/localhost|https?:\/\/tauri\.localhost)/, "アプリの画面は遷移しない");

  await session.click(tab("リリースノート"));
  const releases = await session.waitForText(panel, "模擬サーバーのリリースノート");
  assert.ok(releases.includes("Kiroscribe 99.0.0"), releases);
  await session.click('button[aria-label="Kiroscribe 99.0.0 を GitHub で見る"]');
  await session.waitUntil(() => ctx.work.openedUrls().length === 2, { message: "リリースのページを外部ブラウザで開く" });
  assert.equal(ctx.work.openedUrls()[1], "https://github.com/s-tabana/kiroscribe/releases/tag/v99.0.0");

  // 取得の仕方: 使い方は raw、リリース一覧は GitHub の API
  assert.equal(ctx.server.requestsTo("/github-raw/").length, 1);
  const api = ctx.server.requestsTo("/github-api/")[0];
  assert.equal(api.headers.accept, "application/vnd.github+json");
  assert.match(api.headers["user-agent"], /^Kiroscribe\//);
  await closeHelp(session);

  // 次に開いたとき取得できなければ、前回取得した内容を表示して、そのことを知らせる
  ctx.server.github.guide = null;
  ctx.server.github.releases = null;
  await openHelp(session);
  await session.waitForText(panel, "模擬サーバーから取得した内容です。");
  await session.waitForText(notice, "オンラインの最新版を取得できないため、保存済みの内容を表示しています");
  await session.click(tab("リリースノート"));
  await session.waitForText(panel, "模擬サーバーのリリースノート");
  await session.waitForText(notice, "保存済みの内容を表示しています");
});

test("オンラインの内容を一度も取得できていないときは、アプリに同梱の使い方とリリースノートが表示される", async (ctx) => {
  ctx.server.github.guide = null;
  ctx.server.github.releases = null;
  const session = await ctx.launch();

  await openHelp(session);
  await session.waitForText(panel, bundledGuideHeading(), { timeout: 30_000 });
  await session.waitForText(notice, "オンラインの最新版を取得できないため、アプリに同梱の内容を表示しています");

  await session.click(tab("リリースノート"));
  // 同梱のリリースノート（CHANGELOG.md）には、いまのバージョンの節がある
  const version = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).version;
  const releases = await session.waitForText(panel, version, { timeout: 30_000 });
  assert.ok(releases.includes("現在のバージョン"), releases);
  await session.waitForText(notice, "アプリに同梱の内容を表示しています");

  // オンラインに戻ったら、［再読み込み］で最新の内容になる
  ctx.server.github.releases = [
    { tag_name: "v99.0.0", name: "Kiroscribe 99.0.0", published_at: "2026-12-01T00:00:00Z", body: "- 戻ったあとの内容", draft: false, prerelease: false },
  ];
  await session.click(`${notice} button`);
  await session.waitForText(panel, "戻ったあとの内容");
  await session.waitGone(notice);
});
