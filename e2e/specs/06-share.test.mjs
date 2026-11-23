// 共有: Markdown のコピーと、ファイルへの保存。
// OS のクリップボードと保存ダイアログは WebDriver から扱えないので、E2E 用のビルドの差し替え
// （clipboard.txt への書き出しと、dialogs.json の答え）で確かめる。

import assert from "node:assert/strict";
import fs from "node:fs";
import { test } from "../lib/app.mjs";
import { createNote, pressBarButton, sel } from "../lib/ui.mjs";

const popover = 'div[role="dialog"][aria-label="共有"]';
const shareButton = (text) => `xpath://div[@role="dialog"][@aria-label="共有"]//button[normalize-space(.)="${text}"]`;
const shareStatus = `${popover} [role="status"]`;

async function openShare(session) {
  await session.click("button.share-trigger");
  await session.$(popover);
}

test("[SHR-007][SHR-008][SHR-012] 共有: Markdown をコピーでき、選んだ場所にファイルとして保存できる", async (ctx) => {
  const session = await ctx.launch();
  await createNote(session, { title: "週次定例", memo: "- リリース日を決める" });

  // 議事録がまだなければ、メモを書き出す。書いたばかりで、まだ自動で保存されていない入力も、書き出しに入る
  await (await session.$(sel.memo)).type("\n- 書いた直後の行");
  await openShare(session);
  await session.click(shareButton("Markdown をコピー"));
  await session.waitForText(shareStatus, "Markdown をコピーしました");
  const notesOnly = ctx.work.clipboard();
  assert.match(notesOnly, /^# 週次定例\n/);
  assert.ok(notesOnly.includes("- リリース日を決める"));
  assert.ok(notesOnly.includes("- 書いた直後の行"), `保存の前の入力が、書き出しに入っていない:\n${notesOnly}`);
  await session.click("button.share-trigger"); // 閉じる
  await session.waitGone(popover);

  // 議事録を作ると、既定では議事録だけになる
  await pressBarButton(session, "議事録を作成");
  await session.waitForText(`${sel.minutes}[aria-busy="false"]`, "リリースは 12/5");
  await openShare(session);
  await session.click(shareButton("Markdown をコピー"));
  await session.waitUntil(() => ctx.work.clipboard() !== notesOnly, { message: "クリップボードの内容が変わる" });
  const minutesOnly = ctx.work.clipboard();
  assert.ok(minutesOnly.includes("リリースは 12/5"));
  assert.ok(!minutesOnly.includes("- リリース日を決める"), "既定ではメモを含めない");

  // 保存: ダイアログで選んだパスに書き出す（既定のファイル名は .md）
  const out = ctx.work.file("書き出し.md");
  ctx.work.answerDialogs({ savePath: out });
  await session.click(shareButton("ファイルに保存…"));
  await session.waitForText(shareStatus, `保存しました: ${out}`);
  assert.equal(fs.readFileSync(out, "utf8"), minutesOnly);
  const dialog = ctx.work.dialogs().at(-1);
  assert.equal(dialog.type, "save");
  assert.match(dialog.options.defaultPath, /^\d{4}-\d{2}-\d{2} 週次定例\.md$/, "ファイル名の初期値は「日付 タイトル.md」");
  assert.deepEqual(dialog.options.filters[0], { name: "Markdown", extensions: ["md"] }, "保存ダイアログの既定の種類は Markdown");

  // メモも含めて保存
  await session.click(`xpath://div[@role="dialog"][@aria-label="共有"]//label[.//span[normalize-space(.)="自分のメモ"]]/input`);
  await session.click(shareButton("ファイルに保存…"));
  await session.waitUntil(() => fs.readFileSync(out, "utf8").includes("- リリース日を決める"), {
    message: "メモを含めた内容で上書きされる",
  });
  assert.ok(fs.readFileSync(out, "utf8").includes("リリースは 12/5"));

  // 保存ダイアログを取り消したら、何も書かない
  const cancelled = ctx.work.file("取り消し.md");
  ctx.work.answerDialogs({ savePath: null });
  const dialogsBefore = ctx.work.dialogs().length;
  await session.click(shareButton("ファイルに保存…"));
  await session.waitUntil(() => ctx.work.dialogs().length === dialogsBefore + 1, { message: "保存ダイアログが出る" });
  await session.waitUntil(async () => (await (await session.$(shareButton("ファイルに保存…"))).isEnabled()) === true, {
    message: "操作が終わる",
  });
  assert.equal(fs.existsSync(cancelled), false);

  // Markdown・テキスト以外の拡張子には書き出さない（ほかの種類のファイルを誤って上書きしないため）
  const wrong = ctx.work.file("書き出し.exe");
  ctx.work.answerDialogs({ savePath: wrong });
  await session.click(shareButton("ファイルに保存…"));
  await session.waitForText(shareStatus, "保存できるのは .md / .txt ファイルだけです");
  assert.equal(fs.existsSync(wrong), false);
});
