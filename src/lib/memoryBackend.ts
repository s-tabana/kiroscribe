import type { Backend, Unsubscribe } from "./backend";
import { toSummary } from "./notes";
import {
  DEFAULT_SETTINGS,
  IDLE_STATUS,
  NO_UPDATE,
  type ApiKeyStatus,
  type AudioDevice,
  type AvailableUpdate,
  type ChatMessage,
  type ContentSource,
  type DeltaEvent,
  type ExportOptions,
  type LevelEvent,
  type Note,
  type OpenNoteRequest,
  type PopupPayload,
  type Provider,
  type RecordingErrorEvent,
  type RecordingStatus,
  type SearchFolderStatus,
  type SearchIndexStatus,
  type ReleaseNote,
  type Segment,
  type TranscriptBreak,
  type Settings,
  type Speaker,
  type UpdateStatus,
} from "./types";
import { version as packageVersion } from "../../package.json";
import bundledGuide from "../../docs/guide.md?raw";
import bundledChangelog from "../../CHANGELOG.md?raw";

interface MemoryOptions {
  /** プレビュー用のサンプルノートを入れる */
  seed?: boolean;
  /** 録音を擬似的に再現する（ブラウザでのデザイン確認用）。API キーがなくても録音できる */
  demo?: boolean;
  /** テスト用に確認ダイアログの応答を差し替える */
  confirm?: (message: string) => boolean;
  now?: () => number;
  /** AI 応答を流す間隔（ミリ秒）。テストでは 0 */
  streamDelayMs?: number;
  /** アプリのバージョン（既定は package.json の値） */
  version?: string;
  /** 更新のダウンロードを 10% ずつ進める間隔（ミリ秒）。テストでは 0 */
  updateStepDelayMs?: number;
  /** 更新のインストールの段階（「インストールしています」）を続ける時間（ミリ秒。既定は updateStepDelayMs と同じ） */
  installStepDelayMs?: number;
  /** テスト用に、インストール中の状態を保つ間に待つもの（解決するとインストールを終える） */
  installGate?: () => Promise<void>;
  /** 設定の初期値（指定した項目だけ既定値から変える） */
  settings?: Partial<Settings>;
  /** 「フォルダを追加…」のダイアログで選ぶフォルダ（null なら取り消し。既定はプレビュー用のフォルダ） */
  pickFolder?: () => string | null;
}

/** テストから文字起こしやエラーを発生させるための追加機能 */
export interface MemoryBackend extends Backend {
  emitSegment(noteId: string, speaker: Speaker, text: string): Segment;
  emitError(noteId: string, message: string): void;
  /** 会議の自動検出を再現する（テスト・プレビュー用） */
  simulateMeetingNote(title: string): Note;
  /** タスクトレイのメニューの「会議を検出したら自動で録音」を選んだことにする */
  toggleAutoRecordFromTray(): Promise<void>;
  requestOpenNote(request: OpenNoteRequest): void;
  setPopup(payload: PopupPayload | null): void;
  readonly popupClosed: boolean;
  /** 会議が終わったかの確認への答え（テスト用） */
  readonly recordingCheckAnswers: { noteId: string; deadline: number; keep: boolean }[];
  /** 最後にコピー・保存した内容（テスト用） */
  readonly lastCopied: string | null;
  readonly lastSaved: { fileName: string; markdown: string } | null;

  /** ヘルプの内容を差し替える（既定はリポジトリの docs/guide.md と CHANGELOG.md） */
  setHelpContent(content: { guide?: string; releases?: ReleaseNote[] }): void;
  /** ヘルプの出どころを切り替える。cache / bundled は「取得できなかった」状態の再現 */
  setHelpSource(source: ContentSource): void;
  /** openExternal で開いた URL */
  readonly openedUrls: string[];
  /**
   * 新しい版が公開された状態にする。次の「更新を確認」で見つかる。
   * announce を付けると、自動確認が見つけたときのようにすぐ画面へ知らせる
   * （自動更新がオンなら、続けてダウンロードも始める）
   */
  simulateUpdate(version: string, options?: { notes?: string; announce?: boolean }): void;
  /**
   * メインウィンドウを閉じた（トレイに隠れた）・開いた状態にする。
   * 閉じたときにダウンロード済みの更新があれば、自動でインストールして起動し直した状態を再現する
   */
  setWindowVisible(visible: boolean): void;
  /** 「更新を確認」を失敗させる（null で元に戻す） */
  setUpdateCheckError(message: string | null): void;
  /** 資料の索引の状態を差し替えて知らせる（作っている途中・見つからないフォルダなどの再現） */
  setSearchIndexStatus(status: Partial<SearchIndexStatus>): void;
}

class Emitter<T> {
  private listeners = new Set<(value: T) => void>();
  on(cb: (value: T) => void): Unsubscribe {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }
  emit(value: T) {
    this.listeners.forEach((cb) => cb(value));
  }
}

const DEMO_SCRIPT: Array<[Speaker, string]> = [
  ["them", "では定例を始めます。まずリリース日の確認からお願いします。"],
  ["me", "12月5日で進めたいと思っています。QA の残りは 3 件です。"],
  ["them", "その 3 件は今週中に終わりそうですか？"],
  ["me", "2 件は終わります。1 件は外部 API の回答待ちです。"],
  ["them", "わかりました。回答が来なければ、その機能だけ次のリリースに回しましょう。"],
  ["me", "了解です。価格改定の件は来期に持ち越しでいいですか？"],
  ["them", "はい、来期で。営業には私から伝えておきます。"],
];

/** プレビュー用の議事録（自分のメモの行をそのまま含め、色分けが分かるようにする） */
function demoMinutes(note: Note): string {
  const mine = note.userNotes
    .split("\n")
    .map((l) => l.replace(/^[-*・\s]+/, "").trim())
    .filter(Boolean);
  return [
    `# ${note.title.trim() || "プロダクト定例（リリース確認）"}`,
    "",
    "## 概要",
    "- 12月5日のリリースに向けた残課題と、価格改定の扱いを確認した。",
    "",
    "## 要点",
    ...mine.map((l) => `- ${l}`),
    "- QA の残り 3 件のうち 2 件は今週中に完了見込み。1 件は外部 API の回答待ち。",
    "",
    "## 決定事項",
    "- リリース日は 12月5日で確定。",
    "- 外部 API の回答がなければ、該当機能だけ次のリリースに回す。",
    "",
    "## ToDo",
    "- [ ] 自分: 外部 API の回答状況を確認する（今週中）",
    "- [ ] 相手: 価格改定の持ち越しを営業へ共有する",
  ].join("\n");
}

function demoAnswer(question: string): string {
  if (/決ま|決定/.test(question)) {
    return "決まったことは次の 2 点です。\n\n- リリース日は **12月5日** で確定\n- 外部 API の回答がなければ、その機能だけ次のリリースに回す";
  }
  if (/todo|タスク|宿題/i.test(question)) {
    return "あなたの ToDo は 1 件です。\n\n- [ ] 外部 API の回答状況を確認する（今週中）";
  }
  if (/メール/.test(question)) {
    return "件名: 12/5 リリースの件\n\nお疲れさまです。本日の定例で、リリース日を 12月5日 に確定しました。外部 API の回答次第では一部機能を次回に回します。引き続きよろしくお願いします。";
  }
  return "この会議では、その点について具体的な話は出ていませんでした。";
}

/** CHANGELOG.md を「## バージョン (日付)」の節ごとに分ける（Rust 側 parse_changelog の簡易版） */
function parseChangelog(markdown: string): ReleaseNote[] {
  const releases: ReleaseNote[] = [];
  let current: { note: ReleaseNote; lines: string[] } | null = null;
  const finish = () => {
    if (current) releases.push({ ...current.note, body: current.lines.join("\n").trim() });
    current = null;
  };
  for (const line of markdown.split("\n")) {
    if (!line.startsWith("## ")) {
      current?.lines.push(line);
      continue;
    }
    finish();
    const heading = /^## v?(\d[\w.+-]*)\s*[(（]?([^)）]*)[)）]?\s*$/.exec(line);
    if (!heading) continue;
    const [, version, date] = heading;
    const note: ReleaseNote = {
      tagName: `v${version}`,
      version,
      name: `v${version}`,
      publishedAt: date.trim() || null,
      body: "",
      htmlUrl: null,
    };
    current = { note, lines: [] };
  }
  finish();
  return releases;
}

/** 0.2.0 → 0.3.0（プレビューで「新しい版がある」状態を作るため） */
function nextMinorVersion(version: string): string {
  const [major, minor] = version.split(".").map((part) => Number.parseInt(part, 10) || 0);
  return `${major}.${minor + 1}.0`;
}

const DEMO_UPDATE_NOTES = "### 新しい機能\n\n- 話者ごとに名前を付けられます\n- 議事録を Word 形式で保存できます\n\n### 修正\n\n- 長い会議で文字起こしの表示が遅くなる問題を直しました";

/** 手動で始めた録音のときに、ポップアップの見出しに出す名前（本体の recording_check.rs と同じ） */
const MANUAL_POPUP_LABEL = "Kiroscribe";

const MSG_UPDATE_WHILE_RECORDING = "録音を停止してから更新してください";

const DEMO_DEVICES: AudioDevice[] = [
  { id: "wasapi:default-mic", name: "マイク (Realtek High Definition Audio)", isDefault: true },
  { id: "wasapi:headset", name: "ヘッドセット マイク (USB Audio)", isDefault: false },
];

/**
 * メモリ上のバックエンド。ブラウザでのデザイン確認とテストに使う。
 * データは保存されず、API キーも「設定済み」フラグだけを持つ。
 */
export function createMemoryBackend(options: MemoryOptions = {}): MemoryBackend {
  const now = options.now ?? (() => Date.now());
  const notes = new Map<string, Note>();
  const segments = new Map<string, Segment[]>();
  const breaks = new Map<string, TranscriptBreak[]>();
  let breakId = 0;
  /** 録音中の回の区切り（何も話さずに止めたら消す） */
  let openBreak: TranscriptBreak | null = null;
  let settings: Settings = { ...DEFAULT_SETTINGS, ...options.settings };
  const keys: Record<Provider, boolean> = { openai: false, anthropic: false };
  let counter = 0;
  let segmentId = 0;

  let recording: RecordingStatus = { ...IDLE_STATUS };
  const statusEvents = new Emitter<RecordingStatus>();
  const segmentEvents = new Emitter<Segment>();
  const breakEvents = new Emitter<TranscriptBreak>();
  const levelEvents = new Emitter<LevelEvent>();
  const errorEvents = new Emitter<RecordingErrorEvent>();
  const enhanceEvents = new Emitter<DeltaEvent>();
  const chatEvents = new Emitter<DeltaEvent>();
  const chats = new Map<string, ChatMessage[]>();
  const cancelled = new Set<string>();
  const running = new Set<string>();
  let chatId = 0;
  let lastCopied: string | null = null;
  const noteCreatedEvents = new Emitter<Note>();
  const openNoteEvents = new Emitter<OpenNoteRequest>();
  const openSettingsEvents = new Emitter<void>();
  const popupEvents = new Emitter<PopupPayload>();
  let popup: PopupPayload | null = null;
  let popupClosed = false;
  const recordingCheckAnswers: { noteId: string; deadline: number; keep: boolean }[] = [];
  let lastSaved: { fileName: string; markdown: string } | null = null;

  let appVersion = options.version ?? packageVersion;
  let helpGuide: string = bundledGuide;
  let helpReleases: ReleaseNote[] = parseChangelog(bundledChangelog);
  let helpSource: ContentSource = "network";
  let helpFetchedAt: number | null = null;
  const openedUrls: string[] = [];
  /** 配布元に公開されている新しい版（確認すると見つかる） */
  let published: AvailableUpdate | null = options.demo
    ? { version: nextMinorVersion(appVersion), notes: DEMO_UPDATE_NOTES, publishedAt: now() }
    : null;
  let update: UpdateStatus = { ...NO_UPDATE };
  let updateCheckError: string | null = null;
  const updateEvents = new Emitter<UpdateStatus>();
  const setUpdate = (next: UpdateStatus) => {
    update = next;
    updateEvents.emit({ ...next });
    return { ...next };
  };
  let windowVisible = true;
  /** 自動で更新したあと、次にウィンドウを開いたときに知らせるバージョン */
  let updatedNotice: string | null = null;
  const mainShownEvents = new Emitter<void>();
  const settingsEvents = new Emitter<Settings>();
  /** プレビューで、索引に入っていることにするファイルの件数（フォルダごと） */
  const FOLDER_FILES = 12;
  let searchFolders: SearchFolderStatus[] = [];
  let searchIndex: SearchIndexStatus = {
    state: settings.companion ? "ready" : "off",
    done: 0,
    total: 0,
    files: 0,
    updatedAt: settings.companion ? now() : null,
    folders: [],
    revision: 0,
  };
  const searchIndexEvents = new Emitter<SearchIndexStatus>();
  const publishSearchIndex = (next: Partial<SearchIndexStatus> = {}) => {
    const on = settings.companion;
    searchFolders = searchFolders.map((folder) => ({ ...folder, files: on ? FOLDER_FILES : 0 }));
    searchIndex = {
      ...searchIndex,
      state: on ? "ready" : "off",
      done: 0,
      total: 0,
      updatedAt: on ? now() : null,
      ...next,
      folders: next.folders ?? searchFolders.map((folder) => ({ ...folder })),
      revision: searchIndex.revision + 1,
    };
    searchIndex.files = searchIndex.folders.reduce((sum, folder) => sum + folder.files, 0);
    searchIndexEvents.emit({ ...searchIndex });
    return { ...searchIndex, folders: searchIndex.folders.map((folder) => ({ ...folder })) };
  };
  const UPDATE_BYTES = 8_400_000;
  const updateStep = () =>
    new Promise((resolve) => setTimeout(resolve, options.updateStepDelayMs ?? (options.demo ? 220 : 0)));

  /** 邪魔にならないとき（録音も AI の生成もしておらず、ウィンドウを閉じている）なら、ダウンロード済みの更新を入れる */
  const autoInstall = () => {
    if (!update.ready || !update.available || update.phase !== "idle") return;
    if (windowVisible || recording.state !== "idle" || running.size > 0) return;
    // 実物はここでアプリが終了し、ウィンドウを出さずに新しい版で起動し直される。その後の状態を再現する
    appVersion = update.available.version;
    updatedNotice = appVersion;
    published = null;
    setUpdate({ ...NO_UPDATE });
  };

  /** 自動更新がオンなら、見つけた更新をダウンロードしておく */
  const backgroundDownload = async () => {
    if (!settings.autoUpdate || !update.available || update.ready || update.phase !== "idle") return;
    setUpdate({ ...update, phase: "downloading", downloadedBytes: 0, totalBytes: UPDATE_BYTES });
    for (let step = 1; step <= 10; step += 1) {
      await updateStep();
      setUpdate({ ...update, downloadedBytes: (UPDATE_BYTES / 10) * step });
    }
    // ダウンロード中に設定がオフになっていたら、持たない
    setUpdate({ ...update, phase: "idle", downloadedBytes: 0, totalBytes: null, ready: settings.autoUpdate });
    autoInstall();
  };
  /** 取得できたときは時刻を覚え、取得できない状態（cache）では前回の時刻を返す */
  const helpMeta = () => {
    if (helpSource === "network") helpFetchedAt = now();
    const fetchedAt = helpSource === "bundled" ? null : (helpFetchedAt ?? now() - 3 * 60 * 60 * 1000);
    return { source: helpSource, fetchedAt };
  };

  /** Rust 側 note_to_markdown の簡易版（プレビュー・テスト用） */
  const toMarkdown = (noteId: string, options: ExportOptions) => {
    const note = find(noteId);
    const title = note.title.trim() || "無題のノート";
    const parts = [`# ${title}`, "", new Date(note.createdAt).toLocaleString("ja-JP")];
    const minutes = note.enhancedNotes?.trim();
    if (options.minutes && minutes) parts.push("", minutes);
    if ((options.notes || (options.minutes && !minutes)) && note.userNotes.trim()) {
      parts.push("", "## 自分のメモ", "", note.userNotes.trim());
    }
    const segs = segments.get(noteId) ?? [];
    if (options.transcript && segs.length) {
      parts.push("", "## 文字起こし", "");
      segs.forEach((s) => parts.push(`- **${s.speaker === "me" ? "自分" : "相手"}**: ${s.text}`));
    }
    const date = new Date(note.createdAt);
    const ymd = `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
    return {
      markdown: parts.join("\n") + "\n",
      fileName: `${ymd} ${title.replace(/[\\/:*?"<>|]/g, "_")}.md`,
    };
  };
  let demoTimers: Array<ReturnType<typeof setInterval>> = [];
  let demoIndex = 0;

  const newId = () => `note-${++counter}-${Math.random().toString(36).slice(2, 8)}`;
  const status = (): ApiKeyStatus => ({ ...keys, storage: "ブラウザプレビュー（保存されません）" });
  const find = (id: string) => {
    const note = notes.get(id);
    if (!note) throw `ノートが見つかりません: ${id}`;
    return note;
  };
  const setRecording = (next: RecordingStatus) => {
    recording = next;
    statusEvents.emit({ ...next });
    return { ...next };
  };

  const addSegment = (noteId: string, speaker: Speaker, text: string): Segment => {
    const endedAt = now();
    // 録音を始める前の音声はないので、録音中の回の発話は、その回の区切りより前には始まらない
    const roundStart = openBreak?.noteId === noteId ? openBreak.startedAt : -Infinity;
    const segment: Segment = {
      id: ++segmentId,
      noteId,
      speaker,
      startedAt: Math.min(endedAt, Math.max(endedAt - 2500, roundStart)),
      endedAt,
      text,
    };
    segments.set(noteId, [...(segments.get(noteId) ?? []), segment]);
    segmentEvents.emit(segment);
    return segment;
  };

  const requireAnthropic = () => {
    if (!keys.anthropic && !options.demo) {
      throw "API キーが設定されていません（Anthropic）。設定画面から入力してください";
    }
  };

  /** 文字を少しずつ流す。中止されたら例外 */
  const streamText = async (key: string, text: string, emit: (delta: string) => void) => {
    const chars = Array.from(text);
    for (let i = 0; i < chars.length; i += 4) {
      if (cancelled.has(key)) throw "中止しました";
      emit(chars.slice(i, i + 4).join(""));
      const delay = options.streamDelayMs ?? (options.demo ? 25 : 0);
      await new Promise((resolve) => setTimeout(resolve, delay));
    }
  };

  const run = async <T,>(key: string, busy: string, task: () => Promise<T>): Promise<T> => {
    if (running.has(key)) throw busy;
    running.add(key);
    cancelled.delete(key);
    try {
      return await task();
    } finally {
      running.delete(key);
      cancelled.delete(key);
    }
  };

  const stopDemo = () => {
    demoTimers.forEach(clearInterval);
    demoTimers = [];
  };

  const startDemo = (noteId: string) => {
    stopDemo();
    let speaking: Speaker = DEMO_SCRIPT[demoIndex % DEMO_SCRIPT.length][0];
    demoTimers.push(
      setInterval(() => {
        for (const speaker of ["me", "them"] as Speaker[]) {
          const active = speaker === speaking;
          levelEvents.emit({ speaker, level: active ? 0.04 + Math.random() * 0.14 : Math.random() * 0.01 });
        }
      }, 100),
      setInterval(() => {
        const [speaker, text] = DEMO_SCRIPT[demoIndex % DEMO_SCRIPT.length];
        demoIndex += 1;
        speaking = DEMO_SCRIPT[demoIndex % DEMO_SCRIPT.length][0];
        addSegment(noteId, speaker, text);
      }, 2800),
    );
  };

  if (options.seed) {
    const day = 24 * 60 * 60 * 1000;
    const base = now();
    const samples: Array<Partial<Note> & { offset: number }> = [
      {
        offset: 0,
        title: "プロダクト定例",
        userNotes: "- リリース日 12/5 で確定？\n- QA の人手が足りない\n- 価格改定は来期に持ち越し",
      },
      {
        offset: day + 3 * 60 * 60 * 1000,
        title: "採用面談（バックエンド）",
        userNotes: "- Rust 経験 3 年\n- 前職でストリーミング基盤\n- 入社可能時期: 2 月",
      },
      { offset: 4 * day, title: "", userNotes: "" },
    ];
    for (const sample of samples) {
      const createdAt = base - sample.offset;
      const id = newId();
      notes.set(id, {
        id,
        title: sample.title ?? "",
        userNotes: sample.userNotes ?? "",
        enhancedNotes: null,
        enhancedAt: null,
        createdAt,
        updatedAt: createdAt,
      });
    }
  }

  return {
    isPreview: true,
    async listNotes() {
      return [...notes.values()].sort((a, b) => b.createdAt - a.createdAt).map(toSummary);
    },
    async searchNotes(query) {
      const needle = query.trim().toLowerCase();
      if (!needle) return [];
      const contains = (text: string | null) => !!text && text.toLowerCase().includes(needle);
      return [...notes.values()]
        .sort((a, b) => b.createdAt - a.createdAt)
        .filter(
          (note) =>
            contains(note.title) ||
            contains(note.userNotes) ||
            contains(note.enhancedNotes) ||
            (segments.get(note.id) ?? []).some((segment) => contains(segment.text)),
        )
        .map((note) => note.id);
    },
    async createNote() {
      const t = now();
      const note: Note = {
        id: newId(),
        title: "",
        userNotes: "",
        enhancedNotes: null,
        enhancedAt: null,
        createdAt: t,
        updatedAt: t,
      };
      notes.set(note.id, note);
      return { ...note };
    },
    async getNote(id) {
      return { ...find(id) };
    },
    async updateNote(id, patch) {
      const note = find(id);
      // 空（空白だけ）の議事録を保存したら「議事録なし」に戻す（本体の update_note と同じ）
      const clearsMinutes = patch.enhancedNotes !== undefined && !patch.enhancedNotes.trim();
      const updated: Note = {
        ...note,
        title: patch.title ?? note.title,
        userNotes: patch.userNotes ?? note.userNotes,
        enhancedNotes: clearsMinutes ? null : (patch.enhancedNotes ?? note.enhancedNotes),
        enhancedAt: clearsMinutes ? null : note.enhancedAt,
        updatedAt: Math.max(now(), note.updatedAt),
      };
      notes.set(id, updated);
      return { ...updated };
    },
    async deleteNote(id) {
      find(id);
      if (recording.noteId === id) throw "録音中のノートは削除できません。先に録音を停止してください";
      notes.delete(id);
      segments.delete(id);
      breaks.delete(id);
    },
    async getSettings() {
      return { ...settings };
    },
    async saveSettings(next) {
      const companionBefore = settings.companion;
      settings = {
        ...next,
        language: next.language.trim() || DEFAULT_SETTINGS.language,
        transcriptionModel: next.transcriptionModel.trim() || DEFAULT_SETTINGS.transcriptionModel,
        claudeModel: next.claudeModel.trim() || DEFAULT_SETTINGS.claudeModel,
        myName: next.myName.trim(),
        inputDeviceId: next.inputDeviceId?.trim() || null,
        companionModel: next.companionModel.trim() || DEFAULT_SETTINGS.companionModel,
      };
      if (settings.companion !== companionBefore) publishSearchIndex();
      // 自動更新の切り替えは、すぐに効く（オフにしたら、待っている自動インストールをやめる）
      if (!settings.autoUpdate && update.ready) setUpdate({ ...update, ready: false });
      void backgroundDownload();
      return { ...settings };
    },
    async getApiKeyStatus() {
      return status();
    },
    async setApiKey(provider, value) {
      keys[provider] = value.trim().length > 0;
      return status();
    },
    async confirm(message) {
      return options.confirm ? options.confirm(message) : window.confirm(message);
    },

    async getSearchIndexStatus() {
      return { ...searchIndex, folders: searchIndex.folders.map((folder) => ({ ...folder })) };
    },
    async addSearchFolder() {
      const path = options.pickFolder ? options.pickFolder() : "C:\\Users\\preview\\Documents\\共有資料";
      if (!path) return null;
      // 一覧にあるフォルダなら、何も変えない
      if (searchFolders.some((folder) => folder.path === path)) return this.getSearchIndexStatus();
      searchFolders.push({ path, noContent: false, files: 0, missing: false, truncated: false, unconfirmed: false });
      return publishSearchIndex();
    },
    async removeSearchFolder(path) {
      searchFolders = searchFolders.filter((folder) => folder.path !== path);
      return publishSearchIndex();
    },
    async setSearchFolderNoContent(path, value) {
      const folder = searchFolders.find((f) => f.path === path);
      if (!folder) throw "このフォルダは一覧にありません";
      folder.noContent = value;
      // 調べ直さないので、状態と更新した時刻は変えない
      return publishSearchIndex({ state: searchIndex.state, updatedAt: searchIndex.updatedAt });
    },
    async rebuildSearchIndex() {
      if (!settings.companion) throw "会議の相棒がオフのときは、索引を作り直せません";
      return publishSearchIndex();
    },
    onSearchIndexStatus: (cb, onReady) => {
      const off = searchIndexEvents.on(cb);
      if (onReady) queueMicrotask(onReady);
      return off;
    },
    setSearchIndexStatus(status) {
      if (status.folders) searchFolders = status.folders.map((folder) => ({ ...folder }));
      searchIndex = { ...searchIndex, ...status, revision: status.revision ?? searchIndex.revision + 1 };
      searchIndexEvents.emit({ ...searchIndex });
    },

    async listAudioDevices() {
      return DEMO_DEVICES.map((d) => ({ ...d }));
    },
    async listSegments(noteId) {
      return [...(segments.get(noteId) ?? [])];
    },
    async listTranscriptBreaks(noteId) {
      return [...(breaks.get(noteId) ?? [])];
    },
    async getRecordingStatus() {
      return { ...recording };
    },
    async startRecording(noteId) {
      // 確かめる順と文は、本体（Rust 側の start_for_note）に合わせる
      if (recording.state === "recording" || recording.state === "paused") {
        if (recording.noteId === noteId) return { ...recording };
        throw "別のノートで録音中です。先にそちらの録音を停止してください";
      }
      if (recording.state === "finishing") {
        throw "前の録音の文字起こしを仕上げています。少し待ってから開始してください";
      }
      find(noteId);
      if (!keys.openai && !options.demo) {
        throw "API キーが設定されていません（OpenAI）。設定画面から入力してください";
      }
      // 本体と同じく、文字起こしのあるノートで始めるときは、始める位置に区切りを付ける
      openBreak = null;
      if ((segments.get(noteId)?.length ?? 0) > 0) {
        const list = breaks.get(noteId) ?? [];
        openBreak = { id: ++breakId, noteId, round: list.length + 2, startedAt: now() };
        breaks.set(noteId, [...list, openBreak]);
        breakEvents.emit({ ...openBreak });
      }
      if (options.demo) startDemo(noteId);
      return setRecording({
        noteId,
        state: "recording",
        elapsedMs: 0,
        runningSince: now(),
        sources: settings.captureSystemAudio ? ["me", "them"] : ["me"],
      });
    },
    async pauseRecording() {
      // 本体と同じく、録音していない（仕上げ中を含む）ときだけエラーにする。一時停止中の一時停止は何も変えない
      if (recording.state === "paused") return setRecording({ ...recording });
      if (recording.state !== "recording") throw "録音していません";
      stopDemo();
      ["me", "them"].forEach((speaker) => levelEvents.emit({ speaker: speaker as Speaker, level: 0 }));
      return setRecording({
        ...recording,
        state: "paused",
        elapsedMs: recording.elapsedMs + (now() - (recording.runningSince ?? now())),
        runningSince: null,
      });
    },
    async resumeRecording() {
      // 録音中の再開は何も変えない（本体と同じ）
      if (recording.state === "recording") return setRecording({ ...recording });
      if (recording.state !== "paused" || !recording.noteId) throw "録音していません";
      if (options.demo) startDemo(recording.noteId);
      return setRecording({ ...recording, state: "recording", runningSince: now() });
    },
    async stopRecording() {
      if (recording.state === "idle") return { ...recording };
      stopDemo();
      setRecording({ ...recording, state: "finishing", runningSince: null });
      await new Promise((resolve) => setTimeout(resolve, options.demo ? 900 : 0));
      // 何も話さないまま止めた回の区切りは残さない
      const added = openBreak;
      openBreak = null;
      if (added && !(segments.get(added.noteId) ?? []).some((s) => s.startedAt >= added.startedAt)) {
        breaks.set(added.noteId, (breaks.get(added.noteId) ?? []).filter((b) => b.id !== added.id));
      }
      const stopped = setRecording({ ...IDLE_STATUS });
      autoInstall();
      return stopped;
    },
    onRecordingStatus: (cb) => statusEvents.on(cb),
    onSegment: (cb) => segmentEvents.on(cb),
    onTranscriptBreak: (cb) => breakEvents.on(cb),
    onLevel: (cb) => levelEvents.on(cb),
    onRecordingError: (cb) => errorEvents.on(cb),
    onNoteCreated: (cb) => noteCreatedEvents.on(cb),
    onOpenNote: (cb) => openNoteEvents.on(cb),
    onOpenSettings: (cb) => openSettingsEvents.on(() => cb()),
    async getPopupState() {
      return popup;
    },
    onPopupUpdate: (cb) => popupEvents.on(cb),
    async closePopup() {
      popupClosed = true;
    },
    async openNoteFromPopup(noteId, enhance) {
      openNoteEvents.emit({ noteId, enhance });
      popupClosed = true;
    },
    async openSettingsFromPopup() {
      openSettingsEvents.emit();
      popupClosed = true;
    },
    async answerRecordingCheck(noteId, deadline, keep) {
      recordingCheckAnswers.push({ noteId, deadline, keep });
      const shown = popup;
      // 本体と同じく、いま出している確認への答えだけを受け付ける
      if (shown?.kind !== "confirmEnd" || shown.noteId !== noteId || shown.deadline !== deadline) return;
      if (recording.noteId !== noteId || recording.state === "idle" || recording.state === "finishing") return;
      const { app, title } = shown;
      if (keep) {
        // 自動で始めた録音なら録音中の表示に戻し、手動の録音ならポップアップを閉じる
        if (app === MANUAL_POPUP_LABEL) {
          popup = null;
          popupClosed = true;
        } else {
          this.setPopup({ kind: "recording", app, noteId, title });
        }
        return;
      }
      await this.stopRecording();
      this.setPopup({ kind: "stopped", app, noteId, title });
    },
    get recordingCheckAnswers() {
      return recordingCheckAnswers;
    },
    simulateMeetingNote(title) {
      const t = now();
      const note: Note = {
        id: newId(),
        title,
        userNotes: "",
        enhancedNotes: null,
        enhancedAt: null,
        createdAt: t,
        updatedAt: t,
      };
      notes.set(note.id, note);
      noteCreatedEvents.emit({ ...note });
      return { ...note };
    },
    requestOpenNote(request) {
      openNoteEvents.emit(request);
    },
    setPopup(payload) {
      popup = payload;
      if (payload) popupEvents.emit(payload);
    },
    get popupClosed() {
      return popupClosed;
    },

    enhanceNote(noteId) {
      return run(`enhance:${noteId}`, "このノートの議事録を作成中です", async () => {
        const note = find(noteId);
        requireAnthropic();
        if (!note.userNotes.trim() && !(segments.get(noteId)?.length ?? 0)) {
          throw "メモも文字起こしもないため、議事録を作れません。メモを書くか録音してから試してください";
        }
        const text = demoMinutes(note);
        await streamText(`enhance:${noteId}`, text, (delta) => enhanceEvents.emit({ noteId, delta }));
        const [first, ...rest] = text.split("\n");
        const t = now();
        const updated: Note = {
          ...find(noteId),
          title: note.title.trim() ? note.title : first.replace(/^#\s*/, ""),
          enhancedNotes: rest.join("\n").trim(),
          enhancedAt: t,
          updatedAt: Math.max(t, note.updatedAt),
        };
        notes.set(noteId, updated);
        return { ...updated };
      });
    },
    async cancelAi(noteId, kind) {
      if (running.has(`${kind}:${noteId}`)) cancelled.add(`${kind}:${noteId}`);
    },
    onEnhanceDelta: (cb) => enhanceEvents.on(cb),
    async listChatMessages(noteId) {
      return [...(chats.get(noteId) ?? [])];
    },
    sendChatMessage(noteId, content) {
      return run(`chat:${noteId}`, "回答を作成中です", async () => {
        find(noteId);
        if (!content.trim()) throw "質問を入力してください";
        requireAnthropic();
        const add = (role: ChatMessage["role"], text: string): ChatMessage => {
          const message = { id: ++chatId, noteId, role, content: text, createdAt: now() };
          chats.set(noteId, [...(chats.get(noteId) ?? []), message]);
          return message;
        };
        const question = add("user", content.trim());
        const answer = demoAnswer(content);
        try {
          await streamText(`chat:${noteId}`, answer, (delta) => chatEvents.emit({ noteId, delta }));
        } catch (err) {
          // 回答をもらえなかった質問（失敗・中止）は、履歴に残さない
          chats.set(noteId, (chats.get(noteId) ?? []).filter((message) => message.id !== question.id));
          throw err;
        }
        return add("assistant", answer);
      });
    },
    async clearChatMessages(noteId) {
      chats.delete(noteId);
    },
    onChatDelta: (cb) => chatEvents.on(cb),

    async exportMarkdown(noteId, options) {
      return toMarkdown(noteId, options);
    },
    async copyText(text) {
      lastCopied = text;
      await navigator.clipboard?.writeText(text).catch(() => undefined);
    },
    async saveMarkdown(noteId, options, fileName) {
      lastSaved = { fileName, markdown: toMarkdown(noteId, options).markdown };
      return fileName;
    },
    async saveMeetingRecord(fileName) {
      // プレビューには会議の検出がないので、記録は空
      return { path: fileName, entries: 0 };
    },
    get lastCopied() {
      return lastCopied;
    },
    get lastSaved() {
      return lastSaved;
    },

    emitSegment: addSegment,
    emitError(noteId, message) {
      errorEvents.emit({ noteId, message });
    },

    async getAppVersion() {
      return appVersion;
    },
    async getGuide() {
      return { markdown: helpGuide, ...helpMeta() };
    },
    async getReleaseNotes() {
      return { releases: helpReleases.map((r) => ({ ...r })), ...helpMeta() };
    },
    async openExternal(url) {
      if (!/^https:\/\//i.test(url)) throw "このリンクは開けません（https:// で始まるリンクだけを開けます）";
      openedUrls.push(url);
      if (options.demo) window.open(url, "_blank", "noopener");
    },
    setHelpContent(content) {
      if (content.guide !== undefined) helpGuide = content.guide;
      if (content.releases !== undefined) helpReleases = content.releases;
    },
    setHelpSource(source) {
      helpSource = source;
    },
    get openedUrls() {
      return openedUrls;
    },

    async getUpdateStatus() {
      return { ...update };
    },
    async checkForUpdate() {
      if (update.phase !== "idle") return { ...update };
      if (updateCheckError) throw updateCheckError;
      await new Promise((resolve) => setTimeout(resolve, options.demo ? 600 : 0));
      const ready = update.ready && update.available?.version === published?.version;
      const next = setUpdate({ ...update, available: published ? { ...published } : null, checkedAt: now(), ready });
      void backgroundDownload();
      return next;
    },
    async installUpdate() {
      if (recording.state !== "idle") throw MSG_UPDATE_WHILE_RECORDING;
      if (running.size > 0) throw "AI が議事録や回答を作成しています。終わってから更新してください";
      if (update.phase !== "idle") throw "更新を実行中です";
      const target = update.available;
      if (!target) throw "新しいバージョンは見つかっていません。先に更新を確認してください";

      // ダウンロード済み（自動更新がダウンロードしておいたもの）なら、それを使う
      if (!update.ready) {
        setUpdate({ ...update, phase: "downloading", downloadedBytes: 0, totalBytes: UPDATE_BYTES });
        for (let step = 1; step <= 10; step += 1) {
          await updateStep();
          setUpdate({ ...update, downloadedBytes: (UPDATE_BYTES / 10) * step });
          // ダウンロード中に録音が始まったら、インストールせずに中止する
          if (recording.state !== "idle") {
            setUpdate({ ...update, phase: "idle", downloadedBytes: 0, totalBytes: null });
            throw MSG_UPDATE_WHILE_RECORDING;
          }
        }
      }
      setUpdate({ ...update, phase: "installing" });
      if (options.installGate) await options.installGate();
      else if (options.installStepDelayMs !== undefined) await new Promise((resolve) => setTimeout(resolve, options.installStepDelayMs));
      else await updateStep();
      // 実物はここでアプリが終了し、新しい版で起動し直される。その後の状態を再現する
      appVersion = target.version;
      published = null;
      setUpdate({ ...NO_UPDATE });
    },
    onUpdateStatus: (cb) => updateEvents.on(cb),
    async takeUpdateNotice() {
      if (!windowVisible) return null;
      const version = updatedNotice;
      updatedNotice = null;
      return version;
    },
    onMainShown: (cb) => mainShownEvents.on(() => cb()),
    onSettingsChanged: (cb) => settingsEvents.on(cb),
    async toggleAutoRecordFromTray() {
      settings = { ...settings, autoRecordMeetings: !settings.autoRecordMeetings };
      settingsEvents.emit({ ...settings });
    },
    simulateUpdate(version, { notes, announce } = {}) {
      published = { version, notes: notes ?? null, publishedAt: now() };
      if (!announce) return;
      const ready = update.ready && update.available?.version === version;
      setUpdate({ ...update, available: { ...published }, checkedAt: now(), ready });
      void backgroundDownload();
    },
    setWindowVisible(visible) {
      windowVisible = visible;
      if (visible) mainShownEvents.emit();
      else autoInstall();
    },
    setUpdateCheckError(message) {
      updateCheckError = message;
    },
  };
}
