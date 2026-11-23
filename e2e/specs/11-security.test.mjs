// セキュリティ上の約束のうち、実際のアプリの画面で確かめるもの。

import assert from "node:assert/strict";
import { test } from "../lib/app.mjs";

test("[SEC-006] 画面（WebView）からは、外部に接続できず、外部の画像やスクリプトも読み込めない", async (ctx) => {
  const session = await ctx.launch();
  // 手元の模擬サーバーを「外部」に見立てる（つながれば、模擬サーバーにリクエストが記録される）
  const outside = `${ctx.server.url}/outside`;

  const fetched = await session.executeAsync(
    `async (url) => {
      try {
        await fetch(url, { mode: "no-cors" });
        return "つながった";
      } catch {
        return "止められた";
      }
    }`,
    `${outside}/data.json`,
  );
  assert.equal(fetched, "止められた", "画面から外部へ通信できている");

  for (const [tag, attribute, file] of [
    ["img", "src", "image.png"],
    ["script", "src", "script.js"],
    ["link", "href", "style.css"],
  ]) {
    const loaded = await session.executeAsync(
      `(tag, attribute, url) =>
        new Promise((resolve) => {
          const element = document.createElement(tag);
          if (tag === "link") element.rel = "stylesheet";
          element.onload = () => resolve("読み込めた");
          element.onerror = () => resolve("止められた");
          element[attribute] = url;
          document.head.appendChild(element);
          setTimeout(() => resolve("止められた"), 3000);
        })`,
      tag,
      attribute,
      `${outside}/${file}`,
    );
    assert.equal(loaded, "止められた", `外部の ${tag} を読み込めている`);
  }
  assert.deepEqual(
    ctx.server.requests.filter((request) => request.path.startsWith("/outside")).map((request) => request.path),
    [],
    "画面からのリクエストが、外部（模擬サーバー）に届いている",
  );

  // 画面そのものは、ふつうに使える（アプリ自身と本体との通信は許している）
  await session.click("button.new-note");
  await session.$('input[aria-label="タイトル"]');
});
