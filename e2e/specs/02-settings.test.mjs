// 設定: API キーの保存と、保存したキーが API に正しく届くこと。

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { ROOT, test } from "../lib/app.mjs";
import { ANTHROPIC_KEY, OPENAI_KEY } from "../lib/mock-server.mjs";
import {
  apiKeyBadge,
  barButton,
  closeSettings,
  createNote,
  openSettings,
  pressBarButton,
  saveApiKey,
  saveSettings,
  sel,
  selectOption,
  settingsCheck,
  settingsField,
} from "../lib/ui.mjs";

test(
  "[KEY-006][SEC-002] API キーを保存すると「設定済み」になり、値は画面にもデータベースにも残らず、API には正しいヘッダで届く",
  { apiKeys: false },
  async (ctx) => {
    let session = await ctx.launch();

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

    // アプリと WebDriver のログ、書き出した Markdown にも、キーの値は出ていない
    for (const key of [OPENAI_KEY, ANTHROPIC_KEY]) {
      assert.equal(ctx.work.logs().includes(key), false, "ログにキーの値が出ている");
    }
    await session.click("button.share-trigger");
    await session.click('xpath://div[@role="dialog"][@aria-label="共有"]//button[normalize-space(.)="Markdown をコピー"]');
    await session.waitUntil(() => ctx.work.clipboard() !== null, { message: "クリップボードに書き出される" });
    assert.ok(!ctx.work.clipboard().includes(OPENAI_KEY) && !ctx.work.clipboard().includes(ANTHROPIC_KEY));

    // アプリを起動し直しても、キーは残っている
    session = await ctx.relaunch();
    await openSettings(session);
    assert.equal(await apiKeyBadge(session, "OpenAI"), "設定済み");
    assert.equal(await apiKeyBadge(session, "Anthropic（Claude）"), "設定済み");

    // 削除すると「未設定」に戻る
    await session.click('xpath://input[@aria-label="OpenAI の API キー"]/following-sibling::button[normalize-space(.)="削除"]');
    await session.waitForText(`${sel.settingsDialog} .modal-footer [role="status"]`, "OpenAI のキーを削除しました");
    assert.equal(await apiKeyBadge(session, "OpenAI"), "未設定");
    assert.equal(await apiKeyBadge(session, "Anthropic（Claude）"), "設定済み");
  },
);

test("[SET-002][SET-004][SET-008][SET-009] 設定は「設定を保存」で保存され、再起動後も残り、次の録音と議事録から使われる。保存せずに閉じると捨てる", async (ctx) => {
  let session = await ctx.launch();
  const myName = settingsField("議事録での自分の呼び名", "input");
  const claudeModel = settingsField("Claude のモデル", "select");
  const transcriptionModel = settingsField("モデル", "select");

  // 保存せずに閉じると、変えた内容は捨てる
  await openSettings(session);
  await (await session.$(myName)).type("保存しない名前");
  await selectOption(session, claudeModel, "claude-haiku-5-5");
  await closeSettings(session);
  await openSettings(session);
  assert.equal(await (await session.$(myName)).value(), "");
  assert.equal(await (await session.$(claudeModel)).value(), "claude-opus-5-5");
  assert.equal(await (await session.$(transcriptionModel)).value(), "gpt-4o-transcribe");

  // 「設定を保存」で保存する
  await (await session.$(myName)).type("田中");
  await selectOption(session, claudeModel, "claude-sonnet-5-5");
  await selectOption(session, transcriptionModel, "gpt-4o-mini-transcribe");
  await saveSettings(session);
  await closeSettings(session);

  // 起動し直さなくても、次の録音と議事録から、選んだモデルが使われる
  await createNote(session, { title: "設定のテスト", memo: "- メモ" });
  await pressBarButton(session, "録音を開始");
  await session.waitUntil(() => ctx.server.transcriptions.length > 0, { timeout: 30_000, message: "文字起こしの API が呼ばれる" });
  await pressBarButton(session, "停止");
  await session.$(barButton("録音を再開"), { timeout: 30_000 });
  assert.ok(ctx.server.transcriptions.every((request) => request.multipart.fields.model === "gpt-4o-mini-transcribe"));
  await pressBarButton(session, "議事録を作成");
  await session.waitForText(`${sel.minutes}[aria-busy="false"]`, "リリースは 12/5");
  assert.equal(ctx.server.messages[0].json.model, "claude-sonnet-5-5");
  assert.ok(ctx.server.messages[0].json.messages[0].content.includes("自分（田中）"), "自分の呼び名が、AI に渡す文字起こしに入る");

  // 再起動しても残っている
  session = await ctx.relaunch();
  await openSettings(session);
  assert.equal(await (await session.$(myName)).value(), "田中");
  assert.equal(await (await session.$(claudeModel)).value(), "claude-sonnet-5-5");
  assert.equal(await (await session.$(transcriptionModel)).value(), "gpt-4o-mini-transcribe");
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

test("[KEY-009][SET-011] 設定画面にバージョンが表示され、［キーを発行する］は発行のページを外部ブラウザで開く", async (ctx) => {
  const session = await ctx.launch();
  await openSettings(session);
  const version = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).version;
  await session.waitForText(`${sel.settingsDialog} .about-version`, `Kiroscribe ${version}`);
  assert.equal((await session.text(`${sel.settingsDialog} .about-version`)).trim(), `Kiroscribe ${version}`);

  const pageUrl = await session.url();
  await session.click('button[aria-label="OpenAI のキーを発行する"]');
  await session.waitUntil(() => ctx.work.openedUrls().length === 1, { message: "OpenAI の発行のページを開く" });
  await session.click('button[aria-label="Anthropic（Claude） のキーを発行する"]');
  await session.waitUntil(() => ctx.work.openedUrls().length === 2, { message: "Anthropic の発行のページを開く" });
  assert.deepEqual(ctx.work.openedUrls(), [
    "https://platform.openai.com/api-keys",
    "https://platform.claude.com/settings/keys",
  ]);
  // アプリの画面は、別のページに移らない（設定画面も開いたまま）
  assert.equal(await session.url(), pageUrl);
  await session.$(sel.settingsDialog);
  assert.equal(await session.exists(`${sel.settingsDialog} .modal-footer .error-text`), false);
  // チェックの切り替えも、保存するまでは効かない（閉じると元に戻る）
  await session.click(settingsCheck("会議を検出したら自動で録音・文字起こしする"));
  await closeSettings(session);
  await openSettings(session);
  assert.equal(await (await session.$(settingsCheck("会議を検出したら自動で録音・文字起こしする"))).isSelected(), true);
});
