import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage, useBackend } from "../lib/backend";
import { releaseDateLabel, shortDateTimeLabel } from "../lib/dates";
import type { ContentSource, Guide, HelpTab, ReleaseNote, ReleaseNotes } from "../lib/types";
import { Markdown } from "./Markdown";
import { CloseIcon, ExternalLinkIcon } from "./icons";

interface HelpDialogProps {
  /** 最初に開くタブ */
  initialTab?: HelpTab;
  onClose(): void;
}

const TAB_LABEL: Record<HelpTab, string> = { guide: "使い方", releases: "リリースノート" };
const TABS = Object.keys(TAB_LABEL) as HelpTab[];

/** 読み込みの状態。error は取得のコマンドそのものが失敗したとき（内容は同梱版まで含めて必ず返るので、通常は起きない） */
type Loaded<T> =
  | { state: "loading" }
  | { state: "ready"; data: T }
  | { state: "error"; message: string };

/** ヘルプ画面（使い方・リリースノート）。内容は GitHub から取得し、取得できないときは保存済みか同梱の内容を表示する */
export function HelpDialog({ initialTab = "guide", onClose }: HelpDialogProps) {
  const backend = useBackend();
  const [tab, setTab] = useState<HelpTab>(initialTab);
  const [guide, setGuide] = useState<Loaded<Guide> | null>(null);
  const [releases, setReleases] = useState<Loaded<ReleaseNotes> | null>(null);
  const [version, setVersion] = useState<string | null>(null);
  const [linkError, setLinkError] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  const loadGuide = useCallback(() => {
    setGuide({ state: "loading" });
    backend
      .getGuide()
      .then((data) => setGuide({ state: "ready", data }))
      .catch((err) => setGuide({ state: "error", message: errorMessage(err) }));
  }, [backend]);

  const loadReleases = useCallback(() => {
    setReleases({ state: "loading" });
    backend
      .getReleaseNotes()
      .then((data) => setReleases({ state: "ready", data }))
      .catch((err) => setReleases({ state: "error", message: errorMessage(err) }));
  }, [backend]);

  // 開いたタブの分だけ、最初に開いたときに取得する
  const requested = useRef(new Set<HelpTab>());
  useEffect(() => {
    if (requested.current.has(tab)) return;
    requested.current.add(tab);
    if (tab === "guide") loadGuide();
    else loadReleases();
  }, [tab, loadGuide, loadReleases]);

  useEffect(() => {
    backend
      .getAppVersion()
      .then(setVersion)
      .catch(() => undefined);
  }, [backend]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    panelRef.current?.focus();
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const openLink = useCallback(
    (url: string) => {
      setLinkError(null);
      backend.openExternal(url).catch((err) => setLinkError(errorMessage(err)));
    },
    [backend],
  );

  const current = tab === "guide" ? guide : releases;
  const reload = tab === "guide" ? loadGuide : loadReleases;

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div
        className="modal help-modal"
        role="dialog"
        aria-modal="true"
        aria-label="ヘルプ"
        ref={panelRef}
        tabIndex={-1}
      >
        <header className="panel-header help-header" role="tablist" aria-label="ヘルプの内容">
          {TABS.map((t) => (
            <button
              key={t}
              type="button"
              role="tab"
              id={`help-tab-${t}`}
              aria-selected={tab === t}
              aria-controls="help-panel"
              className={`panel-tab ${tab === t ? "is-active" : ""}`}
              onClick={() => setTab(t)}
            >
              {TAB_LABEL[t]}
            </button>
          ))}
          <button type="button" className="icon-button" onClick={onClose} aria-label="閉じる">
            <CloseIcon />
          </button>
        </header>

        <div
          className="modal-body help-body"
          role="tabpanel"
          id="help-panel"
          aria-labelledby={`help-tab-${tab}`}
        >
          {linkError && (
            <p className="error-text help-link-error" role="alert">
              {linkError}
            </p>
          )}
          {(!current || current.state === "loading") && (
            <p className="help-loading">読み込んでいます…</p>
          )}
          {current?.state === "error" && (
            <p className="error-text" role="alert">
              {current.message}
            </p>
          )}
          {current?.state === "ready" && (
            <SourceNotice
              source={current.data.source}
              fetchedAt={current.data.fetchedAt}
              onReload={reload}
            />
          )}
          {tab === "guide" && guide?.state === "ready" && (
            <Markdown text={guide.data.markdown} onOpenLink={openLink} />
          )}
          {tab === "releases" && releases?.state === "ready" && (
            <ReleaseList
              releases={releases.data.releases}
              currentVersion={version}
              onOpenLink={openLink}
            />
          )}
        </div>
      </div>
    </div>
  );
}

interface SourceNoticeProps {
  source: ContentSource;
  fetchedAt: number | null;
  onReload(): void;
}

/** 最新の内容を取得できなかったときに、表示している内容の出どころを控えめに知らせる */
function SourceNotice({ source, fetchedAt, onReload }: SourceNoticeProps) {
  if (source === "network") return null;
  return (
    <p className="help-source" role="note">
      <span>
        {source === "cache"
          ? "オンラインの最新版を取得できないため、保存済みの内容を表示しています"
          : "オンラインの最新版を取得できないため、アプリに同梱の内容を表示しています"}
        {source === "cache" && fetchedAt !== null && `（${shortDateTimeLabel(fetchedAt)} に取得）`}
      </span>
      <button type="button" className="link-button" onClick={onReload}>
        再読み込み
      </button>
    </p>
  );
}

interface ReleaseListProps {
  releases: ReleaseNote[];
  currentVersion: string | null;
  onOpenLink(url: string): void;
}

function ReleaseList({ releases, currentVersion, onOpenLink }: ReleaseListProps) {
  if (releases.length === 0) return <p className="help-loading">リリースノートはまだありません</p>;
  return (
    <div className="release-list">
      {releases.map((release) => {
        const date = releaseDateLabel(release.publishedAt);
        const url = release.htmlUrl;
        return (
          <article key={release.tagName} className="release" aria-label={release.name}>
            <header className="release-head">
              <h3>{release.name}</h3>
              {release.version === currentVersion && (
                <span className="badge badge-ok">現在のバージョン</span>
              )}
              {date && <time>{date}</time>}
              {url && /^https:\/\//i.test(url) && (
                <button
                  type="button"
                  className="link-button"
                  onClick={() => onOpenLink(url)}
                  title={url}
                  aria-label={`${release.name} を GitHub で見る`}
                >
                  GitHub
                  <ExternalLinkIcon size={13} />
                </button>
              )}
            </header>
            {release.body.trim() ? (
              <Markdown text={release.body} onOpenLink={onOpenLink} />
            ) : (
              <p className="muted">説明はありません</p>
            )}
          </article>
        );
      })}
    </div>
  );
}
