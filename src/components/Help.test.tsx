import { describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { HelpDialog } from "./HelpDialog";
import { Markdown } from "./Markdown";
import { SettingsDialog } from "./SettingsDialog";
import { BackendContext, type Backend } from "../lib/backend";
import { createMemoryBackend } from "../lib/memoryBackend";

const OPENAI_URL = "https://platform.openai.com/api-keys";
const RELEASE_URL = "https://github.com/s-tabana/kiroscribe/releases/tag/v0.3.0";

function helpBackend() {
  const backend = createMemoryBackend({ version: "0.2.0", confirm: () => true });
  backend.setHelpContent({
    guide: [
      "# Kiroscribe の使い方",
      "",
      "会議中は要点だけメモします。",
      "",
      `- [API キーの取得](${OPENAI_URL})`,
      "- [README](./README.md)",
      "- [暗号化されていないページ](http://example.com/)",
      "",
      "![画面の図](https://example.com/screen.png)",
    ].join("\n"),
    releases: [
      {
        tagName: "v0.3.0",
        version: "0.3.0",
        name: "v0.3.0",
        publishedAt: "2026-12-01T03:00:00Z",
        body: "### 新しい機能\n\n- 話者の名前を付けられます",
        htmlUrl: RELEASE_URL,
      },
      {
        tagName: "v0.2.0",
        version: "0.2.0",
        name: "v0.2.0",
        publishedAt: "2026-11-21",
        body: "- 会議の自動検出",
        htmlUrl: null,
      },
    ],
  });
  return backend;
}

function renderWith(backend: Backend, ui: React.ReactElement) {
  render(<BackendContext.Provider value={backend}>{ui}</BackendContext.Provider>);
}

describe("ヘルプ画面", () => {
  it("[HLP-001][HLP-002][HLP-008][HLP-013] サイドバーから開き、使い方とリリースノートを切り替えられる", async () => {
    const user = userEvent.setup();
    renderWith(helpBackend(), <App />);

    await user.click(await screen.findByRole("button", { name: "ヘルプ" }));
    const dialog = await screen.findByRole("dialog", { name: "ヘルプ" });
    expect(within(dialog).getByRole("tab", { name: "使い方" })).toHaveAttribute("aria-selected", "true");
    expect(await within(dialog).findByRole("heading", { name: "Kiroscribe の使い方" })).toBeInTheDocument();
    // 取得できているときは、出どころの注記を出さない
    expect(within(dialog).queryByRole("note")).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("tab", { name: "リリースノート" }));
    expect(within(dialog).getByRole("tab", { name: "リリースノート" })).toHaveAttribute("aria-selected", "true");
    const current = await within(dialog).findByRole("article", { name: "v0.2.0" });
    expect(within(current).getByText("現在のバージョン")).toBeInTheDocument();
    expect(within(current).getByText("2026年11月21日")).toBeInTheDocument();
    expect(within(current).getByText("会議の自動検出")).toBeInTheDocument();
    const newer = within(dialog).getByRole("article", { name: "v0.3.0" });
    expect(within(newer).queryByText("現在のバージョン")).not.toBeInTheDocument();
    expect(within(newer).getByText("話者の名前を付けられます")).toBeInTheDocument();
    expect(within(dialog).queryByRole("heading", { name: "Kiroscribe の使い方" })).not.toBeInTheDocument();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "ヘルプ" })).not.toBeInTheDocument();
  });

  it("[HLP-007] 取得できなかったときは、表示している内容の出どころを知らせる", async () => {
    const user = userEvent.setup();
    const backend = helpBackend();
    backend.setHelpSource("cache");
    renderWith(backend, <HelpDialog onClose={() => undefined} />);

    const note = await screen.findByRole("note");
    expect(note).toHaveTextContent(/オンラインの最新版を取得できないため、保存済みの内容を表示しています（.+ に取得）/);
    expect(screen.getByRole("heading", { name: "Kiroscribe の使い方" })).toBeInTheDocument();

    // 保存済みもないときはアプリ同梱の内容（リリースノートのタブも同じ）
    backend.setHelpSource("bundled");
    await user.click(within(note).getByRole("button", { name: "再読み込み" }));
    expect(await screen.findByText(/アプリに同梱の内容を表示しています/)).toBeInTheDocument();
    await user.click(screen.getByRole("tab", { name: "リリースノート" }));
    await screen.findByRole("article", { name: "v0.2.0" });
    expect(screen.getByRole("note")).toHaveTextContent("アプリに同梱の内容を表示しています");

    // つながったら注記は消える
    backend.setHelpSource("network");
    await user.click(screen.getByRole("button", { name: "再読み込み" }));
    await waitFor(() => expect(screen.queryByRole("note")).not.toBeInTheDocument());
    expect(screen.getByRole("article", { name: "v0.2.0" })).toBeInTheDocument();
  });

  it("[HLP-011] リンクは外部ブラウザで開くコマンドを呼び、アプリの画面は遷移させない", async () => {
    const user = userEvent.setup();
    const backend = helpBackend();
    const openExternal = vi.spyOn(backend, "openExternal");
    renderWith(backend, <HelpDialog onClose={() => undefined} />);

    const link = await screen.findByRole("link", { name: "API キーの取得" });
    expect(link).not.toHaveAttribute("href");
    await user.click(link);
    expect(openExternal).toHaveBeenCalledWith(OPENAI_URL);
    expect(backend.openedUrls).toEqual([OPENAI_URL]);

    // https 以外のリンクは押せない文字のまま。画像は表示しない
    expect(screen.getByText("README")).toBeInTheDocument();
    expect(screen.getByText("暗号化されていないページ")).toBeInTheDocument();
    expect(screen.getAllByRole("link")).toHaveLength(1);
    expect(screen.queryByRole("img")).not.toBeInTheDocument();

    // キーボードでも開ける
    link.focus();
    await user.keyboard("{Enter}");
    expect(openExternal).toHaveBeenCalledTimes(2);

    await user.click(screen.getByRole("tab", { name: "リリースノート" }));
    await user.click(await screen.findByRole("button", { name: "v0.3.0 を GitHub で見る" }));
    expect(openExternal).toHaveBeenLastCalledWith(RELEASE_URL);
  });

  it("[HLP-012] リンクを開けなかったら理由を表示する", async () => {
    const user = userEvent.setup();
    const backend = helpBackend();
    vi.spyOn(backend, "openExternal").mockRejectedValue("ブラウザを開けませんでした");
    renderWith(backend, <HelpDialog onClose={() => undefined} />);

    await user.click(await screen.findByRole("link", { name: "API キーの取得" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("ブラウザを開けませんでした");
  });

  it("[HLP-001][SET-010] 設定画面のリンクからタブを指定して開く。Esc はヘルプだけを閉じる", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    renderWith(helpBackend(), <SettingsDialog onClose={onClose} />);

    await user.click(await screen.findByRole("button", { name: "リリースノート" }));
    const help = await screen.findByRole("dialog", { name: "ヘルプ" });
    expect(within(help).getByRole("tab", { name: "リリースノート" })).toHaveAttribute("aria-selected", "true");
    await within(help).findByRole("article", { name: "v0.3.0" });

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "ヘルプ" })).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "設定" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "使い方" }));
    expect(
      await within(await screen.findByRole("dialog", { name: "ヘルプ" })).findByRole("heading", {
        name: "Kiroscribe の使い方",
      }),
    ).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("既定の内容はリポジトリの使い方と変更履歴（プレビュー用）", async () => {
    const backend = createMemoryBackend();
    const version = await backend.getAppVersion();
    expect((await backend.getGuide()).markdown).toContain("# Kiroscribe の使い方");
    const { releases } = await backend.getReleaseNotes();
    expect(releases.map((r) => r.version)).toContain(version);
    expect(releases.every((r) => r.body.length > 0)).toBe(true);
  });
});

describe("議事録の Markdown", () => {
  it("[SEC-009] リンクと画像は無効のまま（ヘルプ用の指定をしない限り押せない）", () => {
    render(<Markdown text={"[資料](https://example.com/doc) を参照\n\n![図](https://example.com/a.png)"} />);
    expect(screen.getByText("資料")).toBeInTheDocument();
    expect(screen.queryByRole("link")).not.toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });
});

describe("ヘルプ画面: リリースの表示と閉じ方", () => {
  it("[HLP-008] 本文のないリリースは「説明はありません」、リリースが 1 つもなければその旨を表示する", async () => {
    const backend = helpBackend();
    backend.setHelpContent({
      releases: [{ tagName: "v0.3.0", version: "0.3.0", name: "Kiroscribe 0.3.0", publishedAt: null, body: "  ", htmlUrl: null }],
    });
    renderWith(backend, <HelpDialog initialTab="releases" onClose={() => {}} />);
    const release = await screen.findByRole("article", { name: "Kiroscribe 0.3.0" });
    expect(within(release).getByText("説明はありません")).toBeInTheDocument();
    // 公開日と GitHub のリンクは、ないときは出さない
    expect(release.querySelector("time")).toBeNull();
    expect(within(release).queryByRole("button", { name: /GitHub/ })).not.toBeInTheDocument();
    cleanup();

    backend.setHelpContent({ releases: [] });
    renderWith(backend, <HelpDialog initialTab="releases" onClose={() => {}} />);
    expect(await screen.findByText("リリースノートはまだありません")).toBeInTheDocument();
  });

  it("[HLP-013] 画面の外側のクリックと × で閉じる（画面の中のクリックでは閉じない）", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    renderWith(helpBackend(), <HelpDialog onClose={onClose} />);
    const dialog = await screen.findByRole("dialog", { name: "ヘルプ" });
    await within(dialog).findByRole("heading", { name: "Kiroscribe の使い方" });

    fireEvent.mouseDown(dialog);
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.mouseDown(dialog.parentElement as HTMLElement);
    expect(onClose).toHaveBeenCalledTimes(1);
    await user.click(within(dialog).getByRole("button", { name: "閉じる" }));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});

describe("表示する文章の安全性", () => {
  it("[SEC-009] 埋め込まれた HTML やスクリプトは、要素として解釈しない", () => {
    const text = [
      "<script>window.__kiroscribePwned = true</script>",
      "",
      '<img src="x" onerror="window.__kiroscribePwned = true">',
      "",
      '<a href="https://example.com/">生の HTML のリンク</a>',
      "",
      '<iframe src="https://example.com/"></iframe>',
      "",
      "[スクリプトのリンク](javascript:alert(1))",
      "",
      "ふつうの文",
    ].join("\n");
    // リンクを押せるようにしたヘルプの表示でも、議事録・回答の表示でも同じ
    for (const onOpenLink of [undefined, vi.fn()]) {
      const { container } = render(<Markdown text={text} onOpenLink={onOpenLink} />);
      expect(container.querySelector("script, img, iframe, a[href], [onerror]")).toBeNull();
      expect(screen.queryByRole("link")).not.toBeInTheDocument();
      expect(screen.getByText("ふつうの文")).toBeInTheDocument();
      // 生の HTML は、文字として見えるだけ
      expect(container.textContent).toContain("<script>");
      cleanup();
    }
    expect((window as { __kiroscribePwned?: boolean }).__kiroscribePwned).toBeUndefined();
  });
});
