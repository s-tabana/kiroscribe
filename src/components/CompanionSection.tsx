import { useEffect, useState, type ReactNode } from "react";
import { errorMessage, useBackend } from "../lib/backend";
import { hourlyCostText, indexStatusText } from "../lib/companion";
import type { SearchIndexStatus, Settings } from "../lib/types";

interface CompanionSectionProps {
  settings: Settings;
  /** 相棒のモデルの選択メニュー（議事録・チャットのモデルと同じ選択肢） */
  modelSelect: ReactNode;
  onCompanionChange(value: boolean): void;
  onError(message: string): void;
}

/**
 * 設定画面の「会議の相棒」の欄。オンとオフと相棒のモデルは「設定を保存」で保存する。
 * 探すフォルダの追加・外す・「中身を送らない」は、その場で保存する。
 */
export function CompanionSection({ settings, modelSelect, onCompanionChange, onError }: CompanionSectionProps) {
  const backend = useBackend();
  const [index, setIndex] = useState<SearchIndexStatus | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let active = true;
    backend
      .getSearchIndexStatus()
      .then((status) => active && setIndex(status))
      .catch((err) => onError(errorMessage(err)));
    const off = backend.onSearchIndexStatus(setIndex);
    return () => {
      active = false;
      off();
    };
    // onError は表示の場所を変えるだけなので、購読し直さない
  }, [backend]);

  const act = async (task: () => Promise<SearchIndexStatus | null>) => {
    setBusy(true);
    try {
      const next = await task();
      if (next) setIndex(next);
    } catch (err) {
      onError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  const folders = index?.folders ?? [];
  const indexing = index?.state === "building";

  return (
    <section className="settings-section" aria-labelledby="companion-title">
      <h3 id="companion-title">会議の相棒</h3>
      <label className="check">
        <input type="checkbox" checked={settings.companion} onChange={(e) => onCompanionChange(e.target.checked)} />
        <span>
          録音中に、聞かれたことに答える資料を探す（会議の相棒）
          <small>
            {"「あの資料どこ？」のような発言を見つけると、探すフォルダから答えに使える資料を探して、" +
              "文字起こしの発言の間に出します。"}
          </small>
        </span>
      </label>
      <dl className="companion-sends" aria-label="会議の相棒が PC の外に送るもの">
        <dt>送り先</dt>
        <dd>Anthropic（Claude）</dd>
        <dt>送るもの</dt>
        <dd>
          {"資料を求めていそうな発言と、その前の発言（同じノートの 6 件まで）。" +
            "探すフォルダで見つかった資料の題名・場所（フォルダの名前と、そこからの相対のパス）・抜粋" +
            "（「中身を送らない」にしたフォルダは、抜粋を送りません）。" +
            "資料のファイルそのもの・索引・PC の中のフルパスは送りません。"}
        </dd>
        <dt>費用</dt>
        <dd>
          {`お使いの Anthropic の API キーに請求されます。${hourlyCostText(settings.companionModel)}` +
            "（選んでいる相棒のモデルで、多めに見積もった額）"}
        </dd>
      </dl>
      <div className="field-row">
        <label className="field">
          <span>相棒のモデル</span>
          {modelSelect}
        </label>
      </div>

      <div className="companion-folders">
        <h4 id="companion-folders-title">探すフォルダ</h4>
        <p className="hint">
          {"足したフォルダの中の資料の題名・場所・抜粋は、相棒が資料を選ぶときに Anthropic へ送られます。" +
            "フォルダを足すまでは、どのフォルダも探しません。追加と外すは、その場で保存されます。"}
        </p>
        {folders.length > 0 && (
          <ul className="folder-list" aria-labelledby="companion-folders-title">
            {folders.map((folder) => (
              <li key={folder.path} className="folder-row">
                <div className="folder-main">
                  <span className="folder-path" title={folder.path}>
                    {folder.path}
                  </span>
                  <small className="folder-meta">
                    {folder.missing
                      ? "見つかりません"
                      : `${folder.files.toLocaleString("ja-JP")} 件`}
                    {folder.truncated && !folder.missing && "・一部だけ索引に入れています（20,000 件まで）"}
                  </small>
                </div>
                <label className="check folder-no-content">
                  <input
                    type="checkbox"
                    checked={folder.noContent}
                    disabled={busy}
                    onChange={(e) => {
                      const value = e.target.checked;
                      void act(() => backend.setSearchFolderNoContent(folder.path, value));
                    }}
                  />
                  <span>中身を送らない</span>
                </label>
                <button
                  type="button"
                  className="button button-ghost"
                  aria-label={`${folder.path} を外す`}
                  disabled={busy}
                  onClick={() => void act(() => backend.removeSearchFolder(folder.path))}
                >
                  外す
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="folder-actions">
          <button type="button" className="button" disabled={busy} onClick={() => void act(() => backend.addSearchFolder())}>
            フォルダを追加…
          </button>
          <button
            type="button"
            className="button button-ghost"
            disabled={busy || !index || index.state === "off" || indexing}
            onClick={() => void act(() => backend.rebuildSearchIndex())}
          >
            索引を作り直す
          </button>
        </div>
        {index && (
          <p className="hint index-status" aria-live="polite">
            {indexStatusText(index)}
          </p>
        )}
      </div>
    </section>
  );
}
