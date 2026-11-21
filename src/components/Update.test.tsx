import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { MeetingPopup } from "./MeetingPopup";
import { SettingsDialog } from "./SettingsDialog";
import { BackendContext, type Backend } from "../lib/backend";
import { createMemoryBackend } from "../lib/memoryBackend";

const NOTES = "### 新しい機能\n\n- 話者ごとに名前を付けられます";

function updateBackend(confirm: (message: string) => boolean = () => true) {
  return createMemoryBackend({ seed: true, version: "0.2.0", confirm, updateStepDelayMs: 15 });
}

function renderWith(backend: Backend, ui: React.ReactElement) {
  render(<BackendContext.Provider value={backend}>{ui}</BackendContext.Provider>);
}

async function about() {
  return (await screen.findByRole("heading", { name: "このアプリについて" })).closest("section") as HTMLElement;
}

describe("設定画面の「このアプリについて」", () => {
  it("バージョンを表示する", async () => {
    renderWith(updateBackend(), <SettingsDialog onClose={() => undefined} />);
    const section = await about();
    expect(await within(section).findByText("Kiroscribe 0.2.0")).toBeInTheDocument();
    expect(within(section).getByRole("button", { name: "更新を確認" })).toBeEnabled();
    expect(within(section).getByRole("button", { name: "使い方" })).toBeInTheDocument();
    expect(within(section).getByRole("button", { name: "リリースノート" })).toBeInTheDocument();
    expect(within(section).queryByText(/新しいバージョン/)).not.toBeInTheDocument();
  });

  it("更新を確認して、最新ならそう伝える", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    const check = vi.spyOn(backend, "checkForUpdate");
    renderWith(backend, <SettingsDialog onClose={() => undefined} />);
    const section = await about();

    await user.click(within(section).getByRole("button", { name: "更新を確認" }));
    expect(check).toHaveBeenCalledOnce();
    expect(await within(section).findByText("最新のバージョンです")).toBeInTheDocument();
    expect(within(section).queryByRole("button", { name: "更新して再起動" })).not.toBeInTheDocument();
  });

  it("新しいバージョンがあれば知らせ、確認のうえダウンロードの進み具合を表示して更新する", async () => {
    const user = userEvent.setup();
    const confirm = vi.fn(() => true);
    const backend = updateBackend(confirm);
    backend.simulateUpdate("0.3.0", { notes: NOTES });
    const install = vi.spyOn(backend, "installUpdate");
    renderWith(backend, <SettingsDialog onClose={() => undefined} />);
    const section = await about();

    await user.click(within(section).getByRole("button", { name: "更新を確認" }));
    expect(await within(section).findByText(/新しいバージョン/)).toHaveTextContent("新しいバージョン 0.3.0 があります");
    expect(within(section).queryByText("最新のバージョンです")).not.toBeInTheDocument();
    // 変更点（リリースノートの本文）も確かめられる
    expect(within(section).getByText("0.3.0 の変更点")).toBeInTheDocument();
    expect(within(section).getByText("話者ごとに名前を付けられます")).toBeInTheDocument();

    await user.click(within(section).getByRole("button", { name: "更新して再起動" }));
    expect(confirm).toHaveBeenCalledOnce();
    expect(confirm.mock.calls[0]).toEqual([expect.stringContaining("Kiroscribe 0.3.0 に更新します")]);

    const progress = await within(section).findByRole("progressbar", { name: "更新のダウンロード" });
    await waitFor(() => expect(Number(progress.getAttribute("aria-valuenow"))).toBeGreaterThanOrEqual(50));
    expect(within(section).getByText(/更新をダウンロードしています/)).toHaveTextContent(/\d+%/);
    expect(within(section).getByRole("button", { name: "更新を確認" })).toBeDisabled();
    expect(await within(section).findByText(/更新をインストールしています/)).toBeInTheDocument();

    // 実物ではここでアプリが再起動する。プレビュー実装は新しいバージョンになった状態を再現する
    expect(await within(section).findByText("Kiroscribe 0.3.0")).toBeInTheDocument();
    expect(install).toHaveBeenCalledOnce();
    expect(within(section).queryByText(/新しいバージョン/)).not.toBeInTheDocument();
  });

  it("確認できなかったら理由を表示する", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    backend.setUpdateCheckError("更新を確認できませんでした。インターネット接続を確認して、もう一度お試しください");
    renderWith(backend, <SettingsDialog onClose={() => undefined} />);
    const section = await about();

    await user.click(within(section).getByRole("button", { name: "更新を確認" }));
    expect(await within(section).findByText(/インターネット接続を確認して/)).toHaveClass("error-text");
    expect(within(section).getByRole("button", { name: "更新を確認" })).toBeEnabled();
  });

  it("確認ダイアログで取り消したら更新しない", async () => {
    const user = userEvent.setup();
    const backend = updateBackend(() => false);
    backend.simulateUpdate("0.3.0", { announce: true });
    const install = vi.spyOn(backend, "installUpdate");
    renderWith(backend, <SettingsDialog onClose={() => undefined} />);
    const section = await about();

    await user.click(await within(section).findByRole("button", { name: "更新して再起動" }));
    expect(install).not.toHaveBeenCalled();
    expect(within(section).getByText(/新しいバージョン/)).toBeInTheDocument();
  });
});

describe("サイドバーの更新の知らせ", () => {
  it("自動確認で新しいバージョンが見つかると表示し、［更新］で更新できる", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    expect(screen.queryByRole("region", { name: "アプリの更新" })).not.toBeInTheDocument();

    // Rust 側の自動確認が見つけて、イベントで知らせてくる
    act(() => backend.simulateUpdate("0.3.0", { notes: NOTES, announce: true }));
    const banner = await screen.findByRole("region", { name: "アプリの更新" });
    expect(banner).toHaveTextContent("新しいバージョン 0.3.0 があります");

    await user.click(within(banner).getByRole("button", { name: "更新" }));
    expect(await within(banner).findByRole("progressbar")).toBeInTheDocument();
    expect(within(banner).queryByRole("button", { name: "更新" })).not.toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("region", { name: "アプリの更新" })).not.toBeInTheDocument());
    expect(await backend.getAppVersion()).toBe("0.3.0");
  });

  it("設定画面にも同じ状態を表示する", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    backend.simulateUpdate("0.3.0", { announce: true });
    renderWith(backend, <App />);
    await screen.findByRole("region", { name: "アプリの更新" });

    await user.click(screen.getByRole("button", { name: "設定" }));
    const section = await about();
    expect(await within(section).findByText(/新しいバージョン/)).toHaveTextContent("新しいバージョン 0.3.0 があります");
    expect(within(section).getByRole("button", { name: "更新して再起動" })).toBeInTheDocument();
  });

  it("録音中は更新を始めず、録音を止めるよう伝える", async () => {
    const user = userEvent.setup();
    const confirm = vi.fn(() => true);
    const backend = updateBackend(confirm);
    await backend.setApiKey("openai", "sk-test");
    backend.simulateUpdate("0.3.0", { announce: true });
    const install = vi.spyOn(backend, "installUpdate");
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");

    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await waitFor(async () => expect((await backend.getRecordingStatus()).state).toBe("recording"));

    const banner = screen.getByRole("region", { name: "アプリの更新" });
    await user.click(within(banner).getByRole("button", { name: "更新" }));
    expect(await within(banner).findByRole("alert")).toHaveTextContent("録音を停止してから更新してください");
    expect(confirm).not.toHaveBeenCalled();
    expect(install).not.toHaveBeenCalled();

    // 設定画面からも同じ
    await user.click(screen.getByRole("button", { name: "設定" }));
    const section = await about();
    await user.click(await within(section).findByRole("button", { name: "更新して再起動" }));
    expect(await within(section).findByText("録音を停止してから更新してください")).toBeInTheDocument();
    expect(install).not.toHaveBeenCalled();

    // バックエンド側でも断る（画面の確認をすり抜けても、録音は止まらない）
    install.mockRestore();
    await expect(backend.installUpdate()).rejects.toBe("録音を停止してから更新してください");
    expect((await backend.getUpdateStatus()).phase).toBe("idle");
    expect((await backend.getRecordingStatus()).state).toBe("recording");
  });

  it("ウィンドウを閉じている間に見つかった更新も、ウィンドウに戻ると分かる", async () => {
    const backend = updateBackend();
    // 閉じている間のイベントを受け取れなかった場合
    vi.spyOn(backend, "onUpdateStatus").mockReturnValue(() => undefined);
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    act(() => backend.simulateUpdate("0.3.0", { announce: true }));
    expect(screen.queryByRole("region", { name: "アプリの更新" })).not.toBeInTheDocument();

    fireEvent.focus(window);
    expect(await screen.findByRole("region", { name: "アプリの更新" })).toHaveTextContent(
      "新しいバージョン 0.3.0 があります",
    );
  });

  it("会議検出のポップアップには出さない", async () => {
    const backend = updateBackend();
    backend.simulateUpdate("0.3.0", { announce: true });
    backend.setPopup({ kind: "recording", app: "Google Meet", noteId: "n1", title: "Google Meet 11/21 10:00" });
    renderWith(backend, <MeetingPopup />);
    await screen.findByRole("alertdialog", { name: "会議の自動録音" });
    expect(screen.queryByText(/新しいバージョン/)).not.toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "アプリの更新" })).not.toBeInTheDocument();
  });
});
