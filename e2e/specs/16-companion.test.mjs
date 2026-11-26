// 会議の相棒の設定: オンとオフ、送るものと費用の表示、探すフォルダの追加と「中身を送らない」、資料の索引。

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "../lib/app.mjs";
import { closeSettings, openSettings, saveSettings, sel, settingsCheck } from "../lib/ui.mjs";

const SECTION = '//div[@role="dialog"][@aria-labelledby="settings-title"]//section[@aria-labelledby="companion-title"]';
const companion = settingsCheck("録音中に、聞かれたことに答える資料を探す（会議の相棒）");
const button = (text) => `xpath:${SECTION}//button[normalize-space(.)="${text}"]`;
const indexStatus = `xpath:${SECTION}//p[contains(@class, "index-status")]`;
const row = (folder) => `xpath:${SECTION}//li[.//span[normalize-space(.)="${folder}"]]`;
const noContent = (folder) => `${row(folder)}//label[span[normalize-space(.)="中身を送らない"]]/input`;

test(
  "[CMP-036][IDX-001][IDX-034][IDX-011] 相棒は初期値でオン。フォルダを足すとその場で保存して索引に入れ、作り直せる。オフにして保存すると索引を空にする",
  async (ctx) => {
    const folder = path.join(ctx.work.dir, "共有資料");
    fs.mkdirSync(path.join(folder, "経理"), { recursive: true });
    fs.writeFileSync(path.join(folder, "経理", "経費精算の手順.txt"), "申請は月末まで");
    fs.writeFileSync(path.join(folder, "議事録.md"), "# 定例\n予算はＡ案");
    fs.writeFileSync(path.join(folder, "ツール.exe"), "入れない種類");
    const session = await ctx.launch();

    await openSettings(session);
    assert.equal(await (await session.$(companion)).isSelected(), true, "相棒は初期値でオン");
    const sends = await session.text(`xpath:${SECTION}//dl[@aria-label="会議の相棒が PC の外に送るもの"]`);
    assert.match(sends, /Anthropic（Claude）/);
    assert.match(sends, /1 時間の会議で最大 約 0\.03 ドル/);
    await session.waitForText(indexStatus, "索引に入っています: 0 件");

    // フォルダを選ぶダイアログ（本体が出す）で選んだフォルダを足す。「設定を保存」は押さない
    ctx.work.answerDialogs({ folderPath: folder });
    await session.click(button("フォルダを追加…"));
    await session.waitForText(row(folder), "2 件");
    await session.waitForText(indexStatus, "索引に入っています: 2 件");
    assert.ok(
      ctx.work.dialogs().some((dialog) => dialog.type === "folder" && dialog.answer === folder),
      "フォルダを選ぶダイアログを出した",
    );
    assert.equal(await (await session.$(noContent(folder))).isSelected(), false, "「中身を送らない」の初期値はオフ");
    await session.click(noContent(folder));
    await session.waitUntil(async () => (await session.$(noContent(folder))).isSelected(), {
      message: "「中身を送らない」がオンになる",
    });
    assert.ok(fs.existsSync(path.join(ctx.work.dataDir, "search-index.db")), "索引は search-index.db に保存する");

    // 閉じて開き直しても残っている（その場で保存している）
    await closeSettings(session);
    await openSettings(session);
    await session.waitForText(row(folder), "2 件");
    assert.equal(await (await session.$(noContent(folder))).isSelected(), true);

    // 作り直す
    fs.writeFileSync(path.join(folder, "出張.txt"), "宿泊の上限");
    await session.click(button("索引を作り直す"));
    await session.waitForText(row(folder), "3 件");
    await session.waitForText(indexStatus, "索引に入っています: 3 件");

    // 相棒をオフにして保存すると、索引を空にする。フォルダの一覧は残り、作り直しは押せない
    await session.click(companion);
    await saveSettings(session);
    await session.waitForText(indexStatus, "会議の相棒がオフのため、索引は作っていません");
    await session.waitForText(row(folder), "0 件");
    assert.equal(await (await session.$(button("索引を作り直す"))).isEnabled(), false);
    assert.equal(await (await session.$(sel.settingsDialog)).isDisplayed(), true);
  },
);
