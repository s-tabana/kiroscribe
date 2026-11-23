// 画面の操作のうち、いくつものテストで使うもの。
// 要素は、利用者に見えるもの（ラベル・ボタンの文字・役割）で探す。見た目のためのクラス名にはなるべく頼らない。

const SETTINGS_XPATH = '//div[@role="dialog"][@aria-labelledby="settings-title"]';
const NOTE_LIST_XPATH = '//nav[@aria-label="ノート一覧"]';

export const sel = {
  newNote: "button.new-note",
  title: 'input[aria-label="タイトル"]',
  memo: 'textarea[aria-label="自分のメモ"]',
  saveState: ".save-state",
  search: 'input[aria-label="ノートを検索"]',
  noteItems: 'nav[aria-label="ノート一覧"] .note-item',
  toast: '.toast[role="alert"]',
  recordingBar: '[aria-label="録音"]',
  settingsDialog: 'div[role="dialog"][aria-labelledby="settings-title"]',
  helpDialog: 'div[role="dialog"][aria-label="ヘルプ"]',
  minutes: 'section[aria-label="議事録"]',
  updateBanner: 'section[aria-label="アプリの更新"]',
  popup: '[role="alertdialog"][aria-label="会議の自動録音"]',
  updatedNotice: 'section[aria-label="更新の完了"]',
};

/** 録音バーのボタンを指すセレクター */
export const barButton = (text) => `xpath://*[@aria-label="録音"]//button[normalize-space(.)="${text}"]`;

/** サイドバーに出ているノートのタイトル（上から順） */
export async function noteTitles(session) {
  return session.execute(
    `return Array.from(document.querySelectorAll(${JSON.stringify(sel.noteItems)} + " .note-item-title > span:first-child")).map((e) => e.textContent)`,
  );
}

/** メモの保存（入力の少しあとに自動で行われる）が済むまで待つ */
export async function waitSaved(session) {
  await session.waitForText(sel.saveState, "保存しました");
}

/** 新しいノートを作り、タイトルとメモを入力して、保存されるまで待つ */
export async function createNote(session, { title, memo } = {}) {
  const before = (await noteTitles(session)).length;
  await session.click(sel.newNote);
  // 新しいノートの画面に切り替わるまで待つ（一覧に 1 件増え、空のタイトル欄にカーソルがある）。
  // ノートを切り替えると入力欄は作り直されるので、切り替わる前の要素をつかんだままにしない
  await session.waitUntil(
    () =>
      session.execute(`
        const title = document.querySelector(${JSON.stringify(sel.title)});
        const count = document.querySelectorAll(${JSON.stringify(sel.noteItems)}).length;
        return count === ${before + 1} && !!title && title.value === "" && document.activeElement === title;
      `),
    { message: "新しいノートが開く" },
  );
  if (title) await (await session.$(sel.title)).type(title);
  if (memo) await (await session.$(sel.memo)).type(memo);
  if (title || memo) await waitSaved(session);
}

/** サイドバーのノートを開く */
export async function openNote(session, title) {
  await session.click(`xpath:${NOTE_LIST_XPATH}//button[.//span[normalize-space(.)="${title}"]]`);
  await session.waitUntil(async () => (await (await session.$(sel.title)).value()) === title, {
    message: `ノート「${title}」が開く`,
  });
}

export async function openSettings(session) {
  await session.click('button[aria-label="設定"]');
  await session.$(sel.settingsDialog);
}

export async function closeSettings(session) {
  await session.click(`xpath:${SETTINGS_XPATH}//footer//button[normalize-space(.)="閉じる"]`);
  await session.waitGone(sel.settingsDialog);
}

/** 設定画面で API キーを保存する。label は "OpenAI" または "Anthropic（Claude）" */
export async function saveApiKey(session, label, key) {
  const input = `input[aria-label="${label} の API キー"]`;
  await (await session.$(input)).type(key);
  await session.click(`xpath://input[@aria-label="${label} の API キー"]/following-sibling::button[normalize-space(.)="保存"]`);
  await session.waitForText(`${sel.settingsDialog} .modal-footer [role="status"]`, `${label} のキーを保存しました`);
}

/** API キーの状態の表示（"設定済み" または "未設定"） */
export async function apiKeyBadge(session, label) {
  return session.text(`xpath://input[@aria-label="${label} の API キー"]/ancestor::div[contains(@class, "api-key")][1]//span[contains(@class, "badge")]`);
}

/** いま出ているお知らせ（画面上部の帯）の文字。出るまで待つ */
export async function toastText(session, expected, options) {
  return session.waitForText(sel.toast, expected, options);
}

export async function dismissToast(session) {
  await session.click('button[aria-label="お知らせを閉じる"]');
  await session.waitGone(sel.toast);
}

/** 録音バーのボタン（"録音を開始"・"停止"・"議事録を作成" など）を押す */
export async function pressBarButton(session, text) {
  await session.click(`xpath://*[@aria-label="録音"]//button[normalize-space(.)="${text}"]`);
}

/** 文字起こしのパネルに出ている発話: [{ speaker: "自分" | "相手", text }] */
export async function utterances(session) {
  return session.execute(`
    return Array.from(document.querySelectorAll(".transcript-list .utterance")).map((u) => ({
      speaker: u.querySelector(".utterance-meta span").textContent,
      text: Array.from(u.querySelectorAll(".utterance-bubble p")).map((p) => p.textContent).join("\\n"),
    }));
  `);
}

/** 選択メニュー（select）で、値が value の項目を選ぶ（利用者が選んだときと同じく、変更のイベントが起きる） */
export async function selectOption(session, selector, value) {
  await session.$(selector);
  const chosen = await session.execute(
    `
    const xpath = arguments[0].startsWith("xpath:") ? arguments[0].slice(6) : null;
    const select = xpath
      ? document.evaluate(xpath, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue
      : document.querySelector(arguments[0]);
    if (!Array.from(select.options).some((option) => option.value === arguments[1])) return null;
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(select, arguments[1]);
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return select.value;
  `,
    selector,
    value,
  );
  if (chosen !== value) throw new Error(`選択メニュー（${selector}）に、値 ${value} の項目がありません`);
}

/** 設定画面の、ラベルが label の欄の入力部品（input・select）を指すセレクター */
export function settingsField(label, tag = "*") {
  return `xpath:${SETTINGS_XPATH}//label[span[normalize-space(.)="${label}"]]/${tag}[self::input or self::select]`;
}

/** 設定画面の、説明が text で始まるチェックボックスを指すセレクター */
export function settingsCheck(text) {
  return `xpath:${SETTINGS_XPATH}//label[contains(@class, "check")][span[starts-with(normalize-space(.), "${text}")]]/input`;
}

/** 設定画面の「設定を保存」を押して、保存されるまで待つ */
export async function saveSettings(session) {
  await session.click(`xpath:${SETTINGS_XPATH}//footer//button[normalize-space(.)="設定を保存"]`);
  await session.waitForText(`${sel.settingsDialog} .modal-footer [role="status"]`, "設定を保存しました");
}

/** 文字起こしのパネルに出ている発話の数（まとまりではなく、発話 1 件ずつ） */
export async function utteranceCount(session) {
  return session.execute(`return document.querySelectorAll(".transcript-list .utterance-bubble p").length`);
}

/** 文字起こしのパネルを開く（すでに開いていれば何もしない） */
export async function openTranscript(session) {
  const toggle = 'button[title="文字起こしを表示"]';
  if ((await (await session.$(toggle)).attribute("aria-pressed")) !== "true") await session.click(toggle);
  await session.$(".transcript-list");
}
