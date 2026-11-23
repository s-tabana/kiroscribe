// 設定: API キーの保存と、保存したキーが API に正しく届くこと。

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "../lib/app.mjs";
import { ANTHROPIC_KEY, OPENAI_KEY } from "../lib/mock-server.mjs";
import { apiKeyBadge, closeSettings, createNote, openSettings, pressBarButton, saveApiKey, sel } from "../lib/ui.mjs";

test(
  "API キーを保存すると「設定済み」になり、値は画面にもデータベースにも残らず、API には正しいヘッダで届く",
  { apiKeys: false },
  async (ctx) => {
    const session = await ctx.launch();

    await openSettings(session);
    assert.equal(await apiKeyBadge(session, "OpenAI"), "未設定");
    assert.equal(await apiKeyBadge(session, "Anthropic（Claude）"), "未設定");

    await saveApiKey(session, "OpenAI", OPENAI_KEY);
    assert.equal(await apiKeyBadge(session, "OpenAI"), "設定済み");
    assert.equal(await apiKeyBadge(session, "Anthropic（Claude）"), "未設定");
    await saveApiKey(session, "Anthropic（Claude）", ANTHROPIC_KEY);
    assert.equal(await apiKeyBadge(session, "Anthropic（Claude）"), "設定済み");

    // 画面: 入力欄は空に戻り、ページのどこにもキーの値がない
    assert.equal(await (await session.$('input[aria-label="OpenAI の API キー"]')).value(), "");
    assert.equal(await (await session.$('input[aria-label="Anthropic（Claude） の API キー"]')).value(), "");
    const page = await session.source();
    assert.ok(!page.includes(OPENAI_KEY) && !page.includes(ANTHROPIC_KEY), "ページにキーの値が出ている");
    await closeSettings(session);

    // 開き直しても「設定済み」のまま（値は表示されない）
    await openSettings(session);
    assert.equal(await apiKeyBadge(session, "OpenAI"), "設定済み");
    assert.equal(await apiKeyBadge(session, "Anthropic（Claude）"), "設定済み");
    await closeSettings(session);

    // 保存したキーで文字起こしと議事録の API が呼ばれる（模擬サーバーは、キーが違えば 401 を返す）
    await createNote(session, { title: "キーの確認", memo: "- メモ" });
    await pressBarButton(session, "録音を開始");
    await session.waitUntil(() => ctx.server.transcriptions.length > 0, { message: "文字起こしの API が呼ばれる" });
    await pressBarButton(session, "停止");
    await session.$(`xpath://*[@aria-label="録音"]//button[normalize-space(.)="録音を再開"]`);
    for (const request of ctx.server.transcriptions) {
      assert.equal(request.headers.authorization, `Bearer ${OPENAI_KEY}`);
      assert.equal(request.headers["x-api-key"], undefined);
    }

    await pressBarButton(session, "議事録を作成");
    await session.waitForText(`${sel.minutes}[aria-busy="false"]`, "リリースは 12/5");
    assert.equal(ctx.server.messages.length, 1);
    const message = ctx.server.messages[0];
    assert.equal(message.headers["x-api-key"], ANTHROPIC_KEY);
    assert.equal(message.headers.authorization, undefined, "Anthropic には Bearer で送らない");
    assert.ok(message.headers["anthropic-version"], "anthropic-version ヘッダ");
    assert.equal(message.headers["content-type"], "application/json");

    // データベース（と、ウェブビューが保存するデータ）にキーの値が平文で残っていない。
    // E2E 用のビルドは、キーを OS の資格情報ストアの代わりに secrets.json に置くので、それだけは除く
    const files = ctx.work.dataFiles().filter((file) => path.basename(file) !== "secrets.json");
    assert.ok(files.some((file) => path.basename(file) === "kiroscribe.db"), "データベースがある");
    for (const file of files) {
      let bytes;
      try {
        bytes = fs.readFileSync(file);
      } catch (err) {
        // ウェブビューが排他的に開いているファイル（Windows）は読めない。データベースは必ず読む
        if (path.basename(file).startsWith("kiroscribe.db") || !["EBUSY", "EPERM", "EACCES"].includes(err.code)) throw err;
        continue;
      }
      for (const key of [OPENAI_KEY, ANTHROPIC_KEY]) {
        assert.equal(bytes.includes(Buffer.from(key, "utf8")), false, `${file} にキーの値が残っている`);
      }
    }

    // 削除すると「未設定」に戻る
    await openSettings(session);
    await session.click('xpath://input[@aria-label="OpenAI の API キー"]/following-sibling::button[normalize-space(.)="削除"]');
    await session.waitForText(`${sel.settingsDialog} .modal-footer [role="status"]`, "OpenAI のキーを削除しました");
    assert.equal(await apiKeyBadge(session, "OpenAI"), "未設定");
    assert.equal(await apiKeyBadge(session, "Anthropic（Claude）"), "設定済み");
  },
);

test("設定の変更（自分の呼び名など）は保存され、再起動後も残っている", async (ctx) => {
  let session = await ctx.launch();
  await openSettings(session);
  const myName = 'xpath://label[span[normalize-space(.)="議事録での自分の呼び名"]]/input';
  await (await session.$(myName)).type("田中");
  await session.click('xpath://footer//button[normalize-space(.)="設定を保存"]');
  await session.waitForText(`${sel.settingsDialog} .modal-footer [role="status"]`, "設定を保存しました");

  session = await ctx.relaunch();
  await openSettings(session);
  assert.equal(await (await session.$(myName)).value(), "田中");
  // 録音に使うマイクの一覧も出ている（E2E では模擬のマイク）
  await session.waitUntil(
    async () =>
      (await session.execute(`
        const label = Array.from(document.querySelectorAll("label")).find((l) => l.querySelector("span")?.textContent === "マイク");
        const select = label?.querySelector("select");
        return select ? select.options[select.selectedIndex].text : null;
      `)) === "既定のマイク（模擬のマイク）",
    { message: "マイクの一覧に模擬のマイクが出る" },
  );
});
