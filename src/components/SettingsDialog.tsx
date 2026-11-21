import { useEffect, useRef, useState } from "react";
import { errorMessage, useBackend } from "../lib/backend";
import { useUpdater } from "../hooks/useUpdater";
import type { ApiKeyStatus, AudioDevice, HelpTab, Provider, Settings } from "../lib/types";
import { HelpDialog } from "./HelpDialog";
import { Markdown } from "./Markdown";
import { UpdateProgress } from "./UpdateBanner";
import { CloseIcon, KeyIcon, LogoMark } from "./icons";

interface SettingsDialogProps {
  onClose(): void;
}

const CLAUDE_MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];
const TRANSCRIPTION_MODELS = ["gpt-4o-transcribe", "gpt-4o-mini-transcribe", "whisper-1"];
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

  useEffect(() => {
    Promise.all([backend.getSettings(), backend.getApiKeyStatus()])
      .then(([s, k]) => {
        setSettings(s);
        setKeys(k);
      })
      .catch((err) => setMessage({ kind: "error", text: errorMessage(err) }));
  }, [backend]);

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
      setSettings(await backend.saveSettings(settings));
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
                      <input
                        list="transcription-models"
                        value={settings.transcriptionModel}
                        onChange={(e) => update("transcriptionModel", e.target.value)}
                      />
                      <datalist id="transcription-models">
                        {TRANSCRIPTION_MODELS.map((m) => (
                          <option key={m} value={m} />
                        ))}
                      </datalist>
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
                        {"Google Meet・Slack ハドル・Zoom・Teams・Discord がマイクを使い始めたら新しいノートを作って録音し、" +
                          "マイクを使わなくなって 20 秒たったら停止します。"}
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
                      <input
                        list="claude-models"
                        value={settings.claudeModel}
                        onChange={(e) => update("claudeModel", e.target.value)}
                      />
                      <datalist id="claude-models">
                        {CLAUDE_MODELS.map((m) => (
                          <option key={m} value={m} />
                        ))}
                      </datalist>
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

            <AboutSection onOpenHelp={setHelpTab} />
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

interface AboutSectionProps {
  onOpenHelp(tab: HelpTab): void;
}

/** バージョンの表示、更新の確認と実行、ヘルプへのリンク */
function AboutSection({ onOpenHelp }: AboutSectionProps) {
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
              新しいバージョン <strong>{available.version}</strong> があります
            </p>
            <button type="button" className="button button-primary" onClick={() => void updater.install()}>
              更新して再起動
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

      <p className="about-links">
        <button type="button" className="link-button" onClick={() => onOpenHelp("guide")}>
          使い方
        </button>
        <button type="button" className="link-button" onClick={() => onOpenHelp("releases")}>
          リリースノート
        </button>
      </p>
    </section>
  );
}

interface ApiKeyFieldProps {
  provider: Provider;
  label: string;
  purpose: string;
  configured: boolean;
  onSaved(status: ApiKeyStatus, message: string): void;
  onError(message: string): void;
}

function ApiKeyField({ provider, label, purpose, configured, onSaved, onError }: ApiKeyFieldProps) {
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
      <p className="hint">{purpose}</p>
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
