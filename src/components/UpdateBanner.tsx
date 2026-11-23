import type { Updater } from "../hooks/useUpdater";
import { CloseIcon } from "./icons";

interface UpdateProgressProps {
  updater: Updater;
}

/** 更新のダウンロード・インストール中の表示（サイドバーと設定画面で共通） */
export function UpdateProgress({ updater }: UpdateProgressProps) {
  const { status, percent } = updater;
  if (status.phase === "installing") {
    return (
      <p className="update-progress-text">更新をインストールしています。まもなく再起動します…</p>
    );
  }
  return (
    <>
      <p className="update-progress-text">
        更新をダウンロードしています…
        {percent !== null && <span className="update-percent">{percent}%</span>}
      </p>
      <div
        className={`update-progress ${percent === null ? "is-indeterminate" : ""}`}
        role="progressbar"
        aria-label="更新のダウンロード"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent ?? undefined}
      >
        <span style={{ width: `${percent ?? 100}%` }} />
      </div>
    </>
  );
}

/** サイドバー下部の知らせ: 新しいバージョンがあるときだけ出す */
export function UpdateBanner({ updater }: UpdateProgressProps) {
  const { status, message } = updater;
  if (!status.available && status.phase === "idle") return null;
  return (
    <section className="update-banner" aria-label="アプリの更新">
      {status.phase !== "idle" ? (
        <UpdateProgress updater={updater} />
      ) : status.ready ? (
        <div className="update-banner-ready">
          <p>
            <span>新しいバージョン {status.available?.version} の</span>
            <span>準備ができました。</span>
            <span>ウィンドウを閉じると更新します</span>
          </p>
          <button type="button" className="button" onClick={() => void updater.install()}>
            今すぐ更新
          </button>
        </div>
      ) : (
        <div className="update-banner-row">
          <p>
            <span>新しいバージョン {status.available?.version}</span> <span>があります</span>
          </p>
          <button type="button" className="button" onClick={() => void updater.install()}>
            更新
          </button>
        </div>
      )}
      {message?.kind === "error" && (
        <p className="error-text update-banner-error" role="alert">
          {message.text}
        </p>
      )}
    </section>
  );
}

interface UpdatedNoticeProps {
  /** 更新後のバージョン */
  version: string;
  onOpenReleaseNotes(): void;
  onDismiss(): void;
}

/** サイドバー下部の知らせ: 自動で更新したあと、次にウィンドウを開いたときに 1 回だけ出す */
export function UpdatedNotice({ version, onOpenReleaseNotes, onDismiss }: UpdatedNoticeProps) {
  return (
    <section className="update-banner" role="status" aria-label="更新の完了">
      <div className="update-banner-row">
        <p>バージョン {version} に更新しました</p>
        <button type="button" className="icon-button" onClick={onDismiss} aria-label="更新の知らせを閉じる">
          <CloseIcon size={15} />
        </button>
      </div>
      <button type="button" className="link-button" onClick={onOpenReleaseNotes}>
        リリースノート
      </button>
    </section>
  );
}
