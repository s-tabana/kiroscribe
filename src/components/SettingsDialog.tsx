import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { errorMessage, useBackend, type Backend } from "../lib/backend";
import { useUpdater } from "../hooks/useUpdater";
import { shortDateTimeLabel } from "../lib/dates";
import type { ApiKeyStatus, AudioDevice, HelpTab, Provider, Settings } from "../lib/types";
import { HelpDialog } from "./HelpDialog";
import { Markdown } from "./Markdown";
import { UpdateProgress } from "./UpdateBanner";
import { CloseIcon, ExternalLinkIcon, KeyIcon, LogoMark } from "./icons";

interface SettingsDialogProps {
  onClose(): void;
}

interface ModelOption {
  /** API に渡すモデル ID */
  value: string;
  label: string;
}

const CLAUDE_MODELS: ModelOption[] = [
  { value: "claude-opus-5-5", label: "Claude Opus 5.5（標準）" },
  { value: "claude-sonnet-5-5", label: "Claude Sonnet 5.5（速くて安い）" },
  { value: "claude-haiku-5-5", label: "Claude Haiku 5.5（最も速くて安い）" },
  { value: "claude-fable-5-2", label: "Claude Fable 5.2（最も高性能・高価）" },
];
const TRANSCRIPTION_MODELS: ModelOption[] = [
  { value: "gpt-4o-transcribe", label: "gpt-4o-transcribe（標準）" },
  { value: "gpt-4o-mini-transcribe", label: "gpt-4o-mini-transcribe（速くて安い）" },
  { value: "whisper-1", label: "whisper-1" },
];
/** API キーを発行するページ */
const API_KEY_PAGES: Record<Provider, string> = {
  openai: "https://platform.openai.com/api-keys",
  anthropic: "https://platform.claude.com/settings/keys",
};
const LANGUAGES = [
  { value: "ja", label: "日本語" },
  { value: "en", label: "English" },
  { value: "auto", label: "自動判定" },
];

export function SettingsDialog({ onClose }: SettingsDialogProps) {
  const backend = useBackend();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [keys, setKeys] = useState<ApiKeyStatus | null>(null);
  const [message, setMessage] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  const [saving, setSaving] = useState(false);
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [deviceError, setDeviceError] = useState<string | null>(null);
  const [helpTab, setHelpTab] = useState<HelpTab | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    backend
      .listAudioDevices()
      .then(setDevices)
      .catch((err) => setDeviceError(errorMessage(err)));
  }, [backend]);

  /** 保存されている設定（画面の外で変わったときに、どの項目が変わったかを知るため） */
  const saved = useRef<Settings | null>(null);

  useEffect(() => {
    Promise.all([backend.getSettings(), backend.getApiKeyStatus()])
      .then(([s, k]) => {
        saved.current = s;
        setSettings(s);
        setKeys(k);
      })
      .catch((err) => setMessage({ kind: "error", text: errorMessage(err) }));
  }, [backend]);

  // 設定画面を開いている間に、タスクトレイのメニューで設定が変わったら、変わった項目だけを表示に反映する
  // （反映しないと、古い表示のまま「設定を保存」を押したときに、トレイでの切り替えが元に戻ってしまう）。
  // この画面で書きかけている、ほかの項目は変えない
  useEffect(
    () =>
      backend.onSettingsChanged((next) => {
        const before = saved.current;
        saved.current = next;
        setSettings((shown) => {
          if (!shown || !before) return next;
          const merged = { ...shown };
          for (const key of Object.keys(next) as (keyof Settings)[]) {
            if (before[key] !== next[key]) Object.assign(merged, { [key]: next[key] });
          }
          return merged;
        });
      }),
    [backend],
  );

  useEffect(() => {
    panelRef.current?.focus();
  }, []);

  useEffect(() => {
    // 上にヘルプを開いている間の Esc は、ヘルプだけを閉じる
    if (helpTab) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, helpTab]);

  const update = <K extends keyof Settings>(key: K, value: Settings[K]) =>
    setSettings((prev) => (prev ? { ...prev, [key]: value } : prev));

  const save = async () => {
    if (!settings) return;
    setSaving(true);
    try {
      const stored = await backend.saveSettings(settings);
      saved.current = stored;
      setSettings(stored);
      setMessage({ kind: "ok", text: "設定を保存しました" });
    } catch (err) {
      setMessage({ kind: "error", text: errorMessage(err) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
        <div
          className="modal"
          role="dialog"
          aria-modal="true"
          aria-labelledby="settings-title"
          ref={panelRef}
          tabIndex={-1}
        >
          <header className="modal-header">
            <h2 id="settings-title">設定</h2>
            <button type="button" className="icon-button" onClick={onClose} aria-label="閉じる">
              <CloseIcon />
            </button>
          </header>

          <div className="modal-body">
            <section className="settings-section">
              <h3>API キー</h3>
              <p className="hint">
                キーはこの PC の
                <strong>{keys?.storage ?? "資格情報ストア"}</strong>
                に保存され、画面には表示されません。
              </p>
              <ApiKeyField
                provider="openai"
                label="OpenAI"
                purpose="会議音声の文字起こしに使います"
                issueUrl={API_KEY_PAGES.openai}
                configured={keys?.openai ?? false}
                onSaved={(status, text) => {
                  setKeys(status);
                  setMessage({ kind: "ok", text });
                }}
                onError={(text) => setMessage({ kind: "error", text })}
              />
              <ApiKeyField
                provider="anthropic"
                label="Anthropic（Claude）"
                purpose="議事録の作成と AI チャットに使います"
                issueUrl={API_KEY_PAGES.anthropic}
                configured={keys?.anthropic ?? false}
                onSaved={(status, text) => {
                  setKeys(status);
                  setMessage({ kind: "ok", text });
                }}
                onError={(text) => setMessage({ kind: "error", text })}
              />
            </section>

            {settings && (
              <>
                <section className="settings-section">
                  <h3>録音・文字起こし</h3>
                  <label className="field field-full">
                    <span>マイク</span>
                    <select
                      value={settings.inputDeviceId ?? ""}
                      onChange={(e) => update("inputDeviceId", e.target.value || null)}
                    >
                      <option value="">
                        既定のマイク
                        {devices.find((d) => d.isDefault) ? `（${devices.find((d) => d.isDefault)!.name}）` : ""}
                      </option>
                      {devices.map((d) => (
                        <option key={d.id} value={d.id}>
                          {d.name}
                        </option>
                      ))}
                      {settings.inputDeviceId && !devices.some((d) => d.id === settings.inputDeviceId) && (
                        <option value={settings.inputDeviceId}>（見つからないデバイス）</option>
                      )}
                    </select>
                    {deviceError && <small className="error-text">{deviceError}</small>}
                  </label>
                  <div className="field-row">
                    <label className="field">
                      <span>言語</span>
                      <select
                        value={settings.language}
                        onChange={(e) => update("language", e.target.value)}
                      >
                        {LANGUAGES.map((l) => (
                          <option key={l.value} value={l.value}>
                            {l.label}
                          </option>
                        ))}
                      </select>
                    </label>
                    <label className="field">
                      <span>モデル</span>
                      <ModelSelect
                        options={TRANSCRIPTION_MODELS}
                        value={settings.transcriptionModel}
                        onChange={(value) => update("transcriptionModel", value)}
                      />
                    </label>
                  </div>
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={settings.captureSystemAudio}
                      onChange={(e) => update("captureSystemAudio", e.target.checked)}
                    />
                    <span>
                      相手の声（PC から流れる音声）も取り込む
                      <small>
                        {"オンライン会議の相手の声を「相手」として文字起こしします。" +
                          "スピーカーで聞くとマイクにも相手の声が入り二重に記録されるため、ヘッドホンをおすすめします。"}
                      </small>
                    </span>
                  </label>
                </section>

                <section className="settings-section">
                  <h3>会議の自動検出</h3>
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={settings.autoRecordMeetings}
                      onChange={(e) => update("autoRecordMeetings", e.target.checked)}
                    />
                    <span>
                      会議を検出したら自動で録音・文字起こしする
                      <small>
                        {"Google Meet・Slack ハドル・Zoom・Teams・Discord の会議に参加したら新しいノートを作って録音し、" +
                          "会議から抜けたら停止します（抜けたことが画面から分からないときは、20 秒たってから停止します）。"}
                      </small>
                    </span>
                  </label>
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={settings.launchAtLogin}
                      onChange={(e) => update("launchAtLogin", e.target.checked)}
                    />
                    <span>
                      Windows の起動時に Kiroscribe を起動する
                      <small>タスクトレイに常駐し、ウィンドウを閉じても会議の検出と録音を続けます。</small>
                    </span>
                  </label>
                </section>

                <section className="settings-section">
                  <h3>AI 議事録・チャット</h3>
                  <div className="field-row">
                    <label className="field">
                      <span>Claude のモデル</span>
                      <ModelSelect
                        options={CLAUDE_MODELS}
                        value={settings.claudeModel}
                        onChange={(value) => update("claudeModel", value)}
                      />
                    </label>
                    <label className="field">
                      <span>議事録での自分の呼び名</span>
                      <input
                        value={settings.myName}
                        placeholder="自分"
                        onChange={(e) => update("myName", e.target.value)}
                      />
                    </label>
                  </div>
                </section>
              </>
            )}

            <AboutSection
              autoUpdate={settings?.autoUpdate ?? null}
              onAutoUpdateChange={(value) => update("autoUpdate", value)}
              onOpenHelp={setHelpTab}
            />
          </div>

          <footer className="modal-footer">
            {message && (
              <p className={message.kind === "ok" ? "ok-text" : "error-text"} role="status">
                {message.text}
              </p>
            )}
            <button type="button" className="button" onClick={onClose}>
              閉じる
            </button>
            <button
              type="button"
              className="button button-primary"
              onClick={save}
              disabled={!settings || saving}
            >
              {saving ? "保存中…" : "設定を保存"}
            </button>
          </footer>
        </div>
      </div>
      {helpTab && <HelpDialog initialTab={helpTab} onClose={() => setHelpTab(null)} />}
    </>
  );
}

interface ModelSelectProps {
  options: ModelOption[];
  value: string;
  onChange(value: string): void;
}

/** モデルの選択。保存済みの値が一覧にないときは、その値を末尾に出して選んだままにする（別のモデルに置き換えない） */
function ModelSelect({ options, value, onChange }: ModelSelectProps) {
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)}>
      {options.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
      {!options.some((option) => option.value === value) && <option value={value}>{value}</option>}
    </select>
  );
}

interface AboutSectionProps {
  /** 設定「新しいバージョンを自動でインストールする」（設定を読み込むまでは null） */
  autoUpdate: boolean | null;
  onAutoUpdateChange(value: boolean): void;
  onOpenHelp(tab: HelpTab): void;
}

/** バージョンの表示、更新の確認と実行、自動更新の設定、ヘルプへのリンク */
function AboutSection({ autoUpdate, onAutoUpdateChange, onOpenHelp }: AboutSectionProps) {
  const backend = useBackend();
  const updater = useUpdater(backend);
  const [version, setVersion] = useState<string | null>(null);
  const { status, message } = updater;
  const available = status.available;
  const idle = status.phase === "idle";

  // 更新の状態が変わったら読み直す（プレビューでは、更新の完了でバージョンが変わる）
  useEffect(() => {
    backend
      .getAppVersion()
      .then(setVersion)
      .catch(() => undefined);
  }, [backend, status.phase]);

  return (
    <section className="settings-section" aria-labelledby="about-title">
      <h3 id="about-title">このアプリについて</h3>
      <div className="about-row">
        <LogoMark size={30} />
        <strong className="about-version">Kiroscribe {version ?? ""}</strong>
        <button
          type="button"
          className="button"
          onClick={() => void updater.check()}
          disabled={updater.checking || !idle}
        >
          {updater.checking ? "確認中…" : "更新を確認"}
        </button>
      </div>

      <div className="about-status" aria-live="polite">
        {available && idle && (
          <div className="about-update">
            <p>
              新しいバージョン <strong>{available.version}</strong>{" "}
              {status.ready ? "の準備ができました。ウィンドウを閉じると更新します" : "があります"}
            </p>
            <button type="button" className="button button-primary" onClick={() => void updater.install()}>
              {status.ready ? "今すぐ更新" : "更新して再起動"}
            </button>
          </div>
        )}
        {!idle && <UpdateProgress updater={updater} />}
        {message && <p className={message.kind === "ok" ? "ok-text" : "error-text"}>{message.text}</p>}
      </div>

      {available?.notes && idle && (
        <details className="about-notes">
          <summary>{available.version} の変更点</summary>
          <Markdown text={available.notes} />
        </details>
      )}

      {autoUpdate !== null && (
        <label className="check">
          <input type="checkbox" checked={autoUpdate} onChange={(e) => onAutoUpdateChange(e.target.checked)} />
          <span>
            新しいバージョンを自動でインストールする
            <small>
              {"オンにすると、ウィンドウを閉じていて、録音もしていないときに更新します。" +
                "オフのときは、新しいバージョンがあることだけを知らせます。"}
            </small>
          </span>
        </label>
      )}

      <p className="about-links">
        <button type="button" className="link-button" onClick={() => onOpenHelp("guide")}>
          使い方
        </button>
        <button type="button" className="link-button" onClick={() => onOpenHelp("releases")}>
          リリースノート
        </button>
      </p>

      <DebugLogSave />
      <DebugLogSubmit />
    </section>
  );
}

/** デバッグ用のログのファイル名（例: kiroscribe-debug-20261127-1904.zip） */
export function debugBundleFileName(now: Date): string {
  const two = (n: number) => String(n).padStart(2, "0");
  const date = `${now.getFullYear()}${two(now.getMonth() + 1)}${two(now.getDate())}`;
  return `kiroscribe-debug-${date}-${two(now.getHours())}${two(now.getMinutes())}.zip`;
}

/**
 * デバッグ用のログ: 「詳しい記録」の切り替えと、ログをまとめた zip の保存
 * （会議を自動で検出できなかった・録音が止まった などのときに、原因を調べるため）
 */
function DebugLogSave() {
  const backend = useBackend();
  const [busy, setBusy] = useState(false);
  const [verboseUntil, setVerboseUntil] = useState<number | null | undefined>(undefined);
  // 切り替えの途中は、次の切り替えを受け付けない（続けて押すと、答えの順番が入れ替わり、表示と本体が食い違うため）
  const [switching, setSwitching] = useState(false);
  const [message, setMessage] = useState<{ kind: "ok" | "error"; text: string } | null>(null);

  useEffect(() => {
    backend
      .getDebugLogStatus()
      .then((status) => setVerboseUntil(status.verboseUntil))
      .catch(() => setVerboseUntil(null));
  }, [backend]);

  const toggleVerbose = async (enabled: boolean) => {
    setMessage(null);
    setSwitching(true);
    try {
      const status = await backend.setDebugVerbose(enabled);
      setVerboseUntil(status.verboseUntil);
    } catch (err) {
      setMessage({ kind: "error", text: errorMessage(err) });
    } finally {
      setSwitching(false);
    }
  };

  const save = async () => {
    setBusy(true);
    setMessage(null);
    try {
      const path = await backend.saveDebugBundle(debugBundleFileName(new Date()));
      if (path) setMessage({ kind: "ok", text: `デバッグ用のログを保存しました: ${path}` });
    } catch (err) {
      setMessage({ kind: "error", text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="about-record">
      {verboseUntil !== undefined && (
        <label className="check">
          <input
            type="checkbox"
            checked={verboseUntil !== null}
            disabled={switching}
            onChange={(e) => void toggleVerbose(e.target.checked)}
          />
          <span>
            詳しい記録を残す（24 時間）
            <small>
              {verboseUntil !== null
                ? `${shortDateTimeLabel(verboseUntil)} まで、細かい出来事も記録します。時間が過ぎると、自動でオフに戻ります。`
                : "オンにすると、24 時間だけ、細かい出来事（会議の検出で見た窓やタブの名前と部品の名前、文字起こしの 1 回ごとの結果など）も記録します。"}
            </small>
          </span>
        </label>
      )}
      <button type="button" className="link-button" onClick={() => void save()} disabled={busy}>
        デバッグ用のログを保存…
      </button>
      <small className="hint">
        {"不具合の原因を調べるための記録を、1 つの zip にまとめて保存します。" +
          "入るのは、アプリの動きの記録（起動と終了、更新の確認、会議の検出の判断、録音、文字起こしと AI の処理の成否と時間、エラー）、" +
          "会議の検出の記録（判定に使ったアプリの名前と印の名前。「詳しい記録」がオンのときは、印の立っている窓やタブの名前も）、" +
          "アプリと OS のバージョン、設定（API キーは設定済みかどうかだけ）です。" +
          "録音・ノート・文字起こし・議事録・AI チャットの内容と API キーは入りません。" +
          "タブの名前には、会議でないページやメールの件名も入ることがあります。人に渡す前に、zip を開いて、見せたくない内容がないか確かめてください。"}
      </small>
      {message && (
        <p className={message.kind === "ok" ? "ok-text" : "error-text"} role="status">
          {message.text}
        </p>
      )}
    </div>
  );
}

/** 提出の前に出す確認の文 */
export const DEBUG_LOG_SUBMIT_CONFIRM =
  "デバッグ用のログを開発者に送ります。\n\n" +
  "送るもの: アプリの動きの記録、会議の検出の記録（判定に使ったアプリの名前と印の名前。「詳しい記録」がオンのときは、印の立っている窓やタブの名前も）、" +
  "アプリと OS のバージョン、設定（API キーは設定済みかどうかだけ）\n" +
  "送らないもの: 録音・ノート・文字起こし・議事録・AI チャットの内容と API キー\n\n" +
  "提出には、このアプリの導入ごとに作る乱数の ID を添えます。利用者名や端末名は ID に含めません。\n\n" +
  "タブの名前には、会議でないページやメールの件名も入ることがあります。" +
  "送る前に中身を確かめたいときは、取り消して「デバッグ用のログを保存…」で保存し、zip を開いてください。\n\n" +
  "送ったログは開発者だけが見られる場所に置き、30 日で消します。送りますか？";

/** オフラインのときの文 */
export const DEBUG_LOG_SUBMIT_OFFLINE =
  "インターネットにつながっていないため、ログを送れません。接続してから、もう一度お試しください";

type SubmitState =
  | { phase: "idle" }
  | { phase: "confirming" }
  | { phase: "packing" }
  | { phase: "sending"; percent: number }
  | { phase: "done"; id: string }
  | { phase: "error"; message: string };

function submissionBusy(state: SubmitState): boolean {
  return state.phase === "confirming" || state.phase === "packing" || state.phase === "sending";
}

function isOffline(): boolean {
  return typeof navigator !== "undefined" && navigator.onLine === false;
}

/** 設定画面を閉じても、確認から提出の完了まで同じ状態を使う。 */
function createLogSubmission(backend: Backend) {
  const listeners = new Set<() => void>();
  let state: SubmitState = { phase: "idle" };
  const setState = (next: SubmitState) => {
    state = next;
    listeners.forEach((listener) => listener());
  };

  const submit = async () => {
    if (submissionBusy(state)) return;
    if (isOffline()) {
      setState({ phase: "error", message: DEBUG_LOG_SUBMIT_OFFLINE });
      return;
    }
    const previous = state;
    setState({ phase: "confirming" });
    try {
      if (!(await backend.confirm(DEBUG_LOG_SUBMIT_CONFIRM, "デバッグ用のログを提出", "info"))) {
        setState(previous);
        return;
      }
      if (isOffline()) {
        setState({ phase: "error", message: DEBUG_LOG_SUBMIT_OFFLINE });
        return;
      }
      setState({ phase: "packing" });
      const requestId = crypto.getRandomValues(new Uint32Array(4)).join("-");
      let listening = true;
      const unsubscribe = await backend.onLogSubmitProgress((progress) => {
        if (!listening || progress.requestId !== requestId) return;
        const { sent, total } = progress;
        const percent = total > 0 ? Math.min(100, Math.floor((sent / total) * 100)) : 0;
        setState({ phase: "sending", percent });
      });
      let result;
      try {
        result = await backend.submitDebugLogs(requestId);
      } finally {
        listening = false;
        await Promise.resolve(unsubscribe()).catch(() => undefined);
      }
      setState({ phase: "done", id: result.id });
    } catch (err) {
      setState({ phase: "error", message: errorMessage(err) });
    }
  };

  return {
    submit,
    getState: () => state,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

const logSubmissions = new WeakMap<Backend, ReturnType<typeof createLogSubmission>>();

function DebugLogSubmit() {
  const backend = useBackend();
  let submission = logSubmissions.get(backend);
  if (!submission) {
    submission = createLogSubmission(backend);
    logSubmissions.set(backend, submission);
  }
  const state = useSyncExternalStore(submission.subscribe, submission.getState);

  return (
    <div className="about-record">
      <button type="button" className="link-button" onClick={submission.submit} disabled={submissionBusy(state)}>
        デバッグ用のログを提出…
      </button>
      <small className="hint">
        {"「デバッグ用のログを保存…」と同じ zip を、開発者に直接送ります。" +
          "保存したファイルを渡す手間がかかりません。送る前に、送るものを確かめる画面が出ます。"}
      </small>
      <div aria-live="polite">
        {state.phase === "packing" && <p role="status">ログをまとめています…</p>}
        {state.phase === "sending" && (
          <p role="status">
            ログを送っています… {state.percent}%
            <progress max={100} value={state.percent} aria-label="ログの送信" />
          </p>
        )}
        {state.phase === "done" && (
          <p className="ok-text" role="status">
            ログを提出しました。提出の番号: <strong>{state.id}</strong>（問い合わせるときに伝えてください）
            <button
              type="button"
              className="link-button"
              onClick={() => void backend.copyText(state.id).catch(() => undefined)}
            >
              番号をコピー
            </button>
          </p>
        )}
        {state.phase === "error" && (
          <p className="error-text" role="status">
            {state.message}
          </p>
        )}
      </div>
    </div>
  );
}

interface ApiKeyFieldProps {
  provider: Provider;
  label: string;
  purpose: string;
  /** キーを発行するページ（既定のブラウザで開く） */
  issueUrl: string;
  configured: boolean;
  onSaved(status: ApiKeyStatus, message: string): void;
  onError(message: string): void;
}

function ApiKeyField({ provider, label, purpose, issueUrl, configured, onSaved, onError }: ApiKeyFieldProps) {
  const backend = useBackend();
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = async (next: string, doneMessage: string) => {
    setBusy(true);
    try {
      const status = await backend.setApiKey(provider, next);
      setValue("");
      onSaved(status, doneMessage);
    } catch (err) {
      onError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="api-key">
      <div className="api-key-head">
        <KeyIcon size={16} />
        <strong>{label}</strong>
        <span className={`badge ${configured ? "badge-ok" : "badge-muted"}`}>
          {configured ? "設定済み" : "未設定"}
        </span>
      </div>
      <p className="hint">
        {purpose}
        <button
          type="button"
          className="link-button"
          aria-label={`${label} のキーを発行する`}
          onClick={() => backend.openExternal(issueUrl).catch((err) => onError(errorMessage(err)))}
        >
          キーを発行する
          <ExternalLinkIcon size={12} />
        </button>
      </p>
      <form
        className="api-key-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (value.trim()) void submit(value, `${label} のキーを保存しました`);
        }}
      >
        <input
          type="password"
          value={value}
          autoComplete="off"
          spellCheck={false}
          placeholder={configured ? "新しいキーで置き換える" : "キーを貼り付け"}
          aria-label={`${label} の API キー`}
          onChange={(e) => setValue(e.target.value)}
        />
        <button type="submit" className="button" disabled={busy || !value.trim()}>
          保存
        </button>
        {configured && (
          <button
            type="button"
            className="button button-ghost"
            disabled={busy}
            onClick={() => void submit("", `${label} のキーを削除しました`)}
          >
            削除
          </button>
        )}
      </form>
    </div>
  );
}
