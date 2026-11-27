import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { MeetingPopup } from "./MeetingPopup";
import { SettingsDialog } from "./SettingsDialog";
import { BackendContext, type Backend } from "../lib/backend";
import { createMemoryBackend } from "../lib/memoryBackend";

const NOTES = "### 新しい機能\n\n- 話者ごとに名前を付けられます";

/** 自動更新がオフ（既定。新しいバージョンがあることを知らせるだけ）のバックエンド */
function updateBackend(confirm: (message: string) => boolean = () => true, installGate?: () => Promise<void>) {
  return createMemoryBackend({ seed: true, version: "0.2.0", confirm, updateStepDelayMs: 15, installGate });
}

/** 設定で自動更新をオンにしたバックエンド */
function autoUpdateBackend(confirm: (message: string) => boolean = () => true) {
  return createMemoryBackend({
    seed: true,
    version: "0.2.0",
    confirm,
    updateStepDelayMs: 5,
    settings: { autoUpdate: true },
  });
}

const READY_TEXT = "新しいバージョン 0.3.0 の準備ができました。ウィンドウを閉じると更新します";
/** 設定画面の、更新の知らせの文（自動更新のチェックボックスの説明とは区別する） */
const UPDATE_TEXT = /^新しいバージョン\s/;

function renderWith(backend: Backend, ui: React.ReactElement) {
  render(<BackendContext.Provider value={backend}>{ui}</BackendContext.Provider>);
}

async function about() {
  return (await screen.findByRole("heading", { name: "このアプリについて" })).closest("section") as HTMLElement;
}

describe("設定画面の「このアプリについて」", () => {
  it("[SET-011] バージョンを表示する", async () => {
    renderWith(updateBackend(), <SettingsDialog onClose={() => undefined} />);
    const section = await about();
    expect(await within(section).findByText("Kiroscribe 0.2.0")).toBeInTheDocument();
    expect(within(section).getByRole("button", { name: "更新を確認" })).toBeEnabled();
    expect(within(section).getByRole("button", { name: "使い方" })).toBeInTheDocument();
    expect(within(section).getByRole("button", { name: "リリースノート" })).toBeInTheDocument();
    expect(within(section).queryByText(UPDATE_TEXT)).not.toBeInTheDocument();
  });

  it("[UPD-006] 更新を確認して、最新ならそう伝える", async () => {
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

  it("[UPD-004][UPD-007][UPD-009][UPD-012] 新しいバージョンがあれば知らせ、確認のうえダウンロードの進み具合を表示して更新する", async () => {
    const user = userEvent.setup();
    const confirm = vi.fn(() => true);
    // インストール中の表示を確かめ終えるまで、インストールを終えない
    let finishInstall = () => undefined as void;
    const installGate = () => new Promise<void>((resolve) => (finishInstall = resolve));
    const backend = updateBackend(confirm, installGate);
    backend.simulateUpdate("0.3.0", { notes: NOTES });
    const install = vi.spyOn(backend, "installUpdate");
    renderWith(backend, <SettingsDialog onClose={() => undefined} />);
    const section = await about();

    await user.click(within(section).getByRole("button", { name: "更新を確認" }));
    expect(await within(section).findByText(UPDATE_TEXT)).toHaveTextContent("新しいバージョン 0.3.0 があります");
    expect(within(section).queryByText("最新のバージョンです")).not.toBeInTheDocument();
    // 変更点（リリースノートの本文）も確かめられる
    expect(within(section).getByText("0.3.0 の変更点")).toBeInTheDocument();
    expect(within(section).getByText("話者ごとに名前を付けられます")).toBeInTheDocument();

    await user.click(within(section).getByRole("button", { name: "更新して再起動" }));
    expect(confirm).toHaveBeenCalledOnce();
    expect(confirm.mock.calls[0]).toEqual([expect.stringContaining("Kiroscribe 0.3.0 に更新します")]);
    expect(confirm.mock.calls[0]).toEqual([expect.stringContaining("ダウンロードが終わると")]);

    const progress = await within(section).findByRole("progressbar", { name: "更新のダウンロード" });
    await waitFor(() => expect(Number(progress.getAttribute("aria-valuenow"))).toBeGreaterThanOrEqual(50));
    expect(within(section).getByText(/更新をダウンロードしています/)).toHaveTextContent(/\d+%/);
    expect(within(section).getByRole("button", { name: "更新を確認" })).toBeDisabled();
    expect(await within(section).findByText(/更新をインストールしています/)).toBeInTheDocument();
    finishInstall();

    // 実物ではここでアプリが再起動する。プレビュー実装は新しいバージョンになった状態を再現する
    expect(await within(section).findByText("Kiroscribe 0.3.0")).toBeInTheDocument();
    expect(install).toHaveBeenCalledOnce();
    expect(within(section).queryByText(UPDATE_TEXT)).not.toBeInTheDocument();
  });

  it("[UPD-006] 確認できなかったら理由を表示する", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    backend.setUpdateCheckError("更新を確認できませんでした。インターネット接続を確認して、もう一度お試しください");
    renderWith(backend, <SettingsDialog onClose={() => undefined} />);
    const section = await about();

    await user.click(within(section).getByRole("button", { name: "更新を確認" }));
    expect(await within(section).findByText(/インターネット接続を確認して/)).toHaveClass("error-text");
    expect(within(section).getByRole("button", { name: "更新を確認" })).toBeEnabled();
  });

  it("[UPD-007] 確認ダイアログで取り消したら更新しない", async () => {
    const user = userEvent.setup();
    const backend = updateBackend(() => false);
    backend.simulateUpdate("0.3.0", { announce: true });
    const install = vi.spyOn(backend, "installUpdate");
    renderWith(backend, <SettingsDialog onClose={() => undefined} />);
    const section = await about();

    await user.click(await within(section).findByRole("button", { name: "更新して再起動" }));
    expect(install).not.toHaveBeenCalled();
    expect(within(section).getByText(UPDATE_TEXT)).toBeInTheDocument();
  });
});

describe("サイドバーの更新の知らせ（自動更新がオフ＝既定）", () => {
  it("[UPD-004][UPD-007][UPD-020] 自動確認で新しいバージョンが見つかると表示し、［更新］で更新できる", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    expect(screen.queryByRole("region", { name: "アプリの更新" })).not.toBeInTheDocument();

    // Rust 側の自動確認が見つけて、イベントで知らせてくる
    act(() => backend.simulateUpdate("0.3.0", { notes: NOTES, announce: true }));
    const banner = await screen.findByRole("region", { name: "アプリの更新" });
    expect(banner).toHaveTextContent("新しいバージョン 0.3.0 があります");
    // 知らせるだけで、ダウンロードはしない
    expect(within(banner).queryByRole("progressbar")).not.toBeInTheDocument();
    expect(await backend.getUpdateStatus()).toMatchObject({ phase: "idle", ready: false });
    // ウィンドウを閉じても、自動では更新しない
    act(() => backend.setWindowVisible(false));
    act(() => backend.setWindowVisible(true));
    expect(await backend.getAppVersion()).toBe("0.2.0");
    expect(screen.queryByRole("status", { name: "更新の完了" })).not.toBeInTheDocument();

    await user.click(within(banner).getByRole("button", { name: "更新" }));
    expect(await within(banner).findByRole("progressbar")).toBeInTheDocument();
    expect(within(banner).queryByRole("button", { name: "更新" })).not.toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("region", { name: "アプリの更新" })).not.toBeInTheDocument());
    expect(await backend.getAppVersion()).toBe("0.3.0");
  });

  it("[UPD-004] 設定画面にも同じ状態を表示する", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    backend.simulateUpdate("0.3.0", { announce: true });
    renderWith(backend, <App />);
    await screen.findByRole("region", { name: "アプリの更新" });

    await user.click(screen.getByRole("button", { name: "設定" }));
    const section = await about();
    expect(await within(section).findByText(UPDATE_TEXT)).toHaveTextContent("新しいバージョン 0.3.0 があります");
    expect(within(section).getByRole("button", { name: "更新して再起動" })).toBeInTheDocument();
  });

  it("[UPD-008] 録音中は更新を始めず、録音を止めるよう伝える", async () => {
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

  it("[UPD-016] ウィンドウを閉じている間に見つかった更新も、ウィンドウに戻ると分かる", async () => {
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

  it("[POP-013] 会議検出のポップアップには出さない", async () => {
    const backend = updateBackend();
    backend.simulateUpdate("0.3.0", { announce: true });
    backend.setPopup({ kind: "recording", app: "Google Meet", noteId: "n1", title: "Google Meet 11/21 10:00" });
    renderWith(backend, <MeetingPopup />);
    await screen.findByRole("alertdialog", { name: "会議の自動録音" });
    expect(screen.queryByText(/新しいバージョン/)).not.toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "アプリの更新" })).not.toBeInTheDocument();
  });
});

describe("自動更新がオンのとき", () => {
  it("[UPD-021][UPD-023] 見つけた更新をダウンロードしておき、ウィンドウを閉じると更新すると知らせる", async () => {
    const user = userEvent.setup();
    const confirm = vi.fn(() => true);
    const backend = autoUpdateBackend(confirm);
    const install = vi.spyOn(backend, "installUpdate");
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");

    act(() => backend.simulateUpdate("0.3.0", { notes: NOTES, announce: true }));
    const banner = await screen.findByRole("region", { name: "アプリの更新" });
    await waitFor(() => expect(banner).toHaveTextContent(READY_TEXT));
    expect(within(banner).queryByRole("progressbar")).not.toBeInTheDocument();
    // ウィンドウを開いている間は、勝手に更新しない
    expect(await backend.getAppVersion()).toBe("0.2.0");
    expect(install).not.toHaveBeenCalled();

    // ［今すぐ更新］で、ダウンロード済みのファイルを使って更新できる
    await user.click(within(banner).getByRole("button", { name: "今すぐ更新" }));
    expect(confirm).toHaveBeenCalledOnce();
    expect(confirm.mock.calls[0]).toEqual([expect.stringContaining("Kiroscribe 0.3.0 に更新します")]);
    expect(confirm.mock.calls[0]).toEqual([expect.not.stringContaining("ダウンロード")]);
    await waitFor(() => expect(screen.queryByRole("region", { name: "アプリの更新" })).not.toBeInTheDocument());
    expect(install).toHaveBeenCalledOnce();
    expect(await backend.getAppVersion()).toBe("0.3.0");
    // 自分で更新したときは、「更新しました」の知らせは出さない
    expect(await backend.takeUpdateNotice()).toBeNull();
  });

  it("[UPD-021][UPD-023] 設定画面にも同じ状態を表示し、手動の確認で見つけた更新もダウンロードしておく", async () => {
    const user = userEvent.setup();
    const backend = autoUpdateBackend();
    backend.simulateUpdate("0.3.0", { notes: NOTES });
    renderWith(backend, <SettingsDialog onClose={() => undefined} />);
    const section = await about();

    await user.click(within(section).getByRole("button", { name: "更新を確認" }));
    await waitFor(() => expect(within(section).getByText(UPDATE_TEXT)).toHaveTextContent(READY_TEXT));
    expect(within(section).getByRole("button", { name: "今すぐ更新" })).toBeInTheDocument();
    expect(within(section).queryByRole("button", { name: "更新して再起動" })).not.toBeInTheDocument();
    expect(within(section).getByText("0.3.0 の変更点")).toBeInTheDocument();
  });

  it("[UPD-022][UPD-025] ウィンドウを閉じると自動で更新し、次に開いたときに 1 回だけ知らせる", async () => {
    const user = userEvent.setup();
    const confirm = vi.fn(() => true);
    const backend = autoUpdateBackend(confirm);
    const install = vi.spyOn(backend, "installUpdate");
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    expect(screen.queryByRole("status", { name: "更新の完了" })).not.toBeInTheDocument();

    act(() => backend.simulateUpdate("0.3.0", { announce: true }));
    await waitFor(() => expect(screen.getByRole("region", { name: "アプリの更新" })).toHaveTextContent(READY_TEXT));

    // ウィンドウを閉じる（トレイに隠れる）と、確認なしで更新される
    act(() => backend.setWindowVisible(false));
    expect(await backend.getAppVersion()).toBe("0.3.0");
    expect(confirm).not.toHaveBeenCalled();
    expect(install).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByRole("region", { name: "アプリの更新" })).not.toBeInTheDocument());
    // 閉じている間は知らせを受け取らない（開いたときのために取っておく）
    expect(await backend.takeUpdateNotice()).toBeNull();
    expect(screen.queryByRole("status", { name: "更新の完了" })).not.toBeInTheDocument();

    // 次にウィンドウを開いたときに知らせる
    act(() => backend.setWindowVisible(true));
    const notice = await screen.findByRole("status", { name: "更新の完了" });
    expect(notice).toHaveTextContent("バージョン 0.3.0 に更新しました");
    expect(within(notice).getByRole("button", { name: "リリースノート" })).toBeInTheDocument();

    // 閉じたら、ウィンドウを開き直しても出さない
    await user.click(within(notice).getByRole("button", { name: "更新の知らせを閉じる" }));
    expect(screen.queryByRole("status", { name: "更新の完了" })).not.toBeInTheDocument();
    act(() => backend.setWindowVisible(false));
    act(() => backend.setWindowVisible(true));
    await waitFor(async () => expect(await backend.takeUpdateNotice()).toBeNull());
    expect(screen.queryByRole("status", { name: "更新の完了" })).not.toBeInTheDocument();
  });

  it("[UPD-025] 更新の知らせからリリースノートを開ける。知らせは 1 回だけで、画面を作り直しても出ない", async () => {
    const user = userEvent.setup();
    const backend = autoUpdateBackend();
    backend.simulateUpdate("0.3.0", { announce: true });
    await waitFor(async () => expect((await backend.getUpdateStatus()).ready).toBe(true));
    backend.setWindowVisible(false);
    backend.setWindowVisible(true);

    const first = render(
      <BackendContext.Provider value={backend}>
        <App />
      </BackendContext.Provider>,
    );
    const notice = await screen.findByRole("status", { name: "更新の完了" });
    expect(notice).toHaveTextContent("バージョン 0.3.0 に更新しました");

    await user.click(within(notice).getByRole("button", { name: "リリースノート" }));
    const dialog = await screen.findByRole("dialog", { name: "ヘルプ" });
    expect(within(dialog).getByRole("tab", { name: "リリースノート" })).toHaveAttribute("aria-selected", "true");
    expect(screen.queryByRole("status", { name: "更新の完了" })).not.toBeInTheDocument();

    first.unmount();
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    expect(screen.queryByRole("status", { name: "更新の完了" })).not.toBeInTheDocument();
  });

  it("[UPD-022] 録音中はウィンドウを閉じても更新せず、録音を止めると更新する", async () => {
    const user = userEvent.setup();
    const backend = autoUpdateBackend();
    await backend.setApiKey("openai", "sk-test");
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    act(() => backend.simulateUpdate("0.3.0", { announce: true }));
    await waitFor(() => expect(screen.getByRole("region", { name: "アプリの更新" })).toHaveTextContent(READY_TEXT));

    await user.click(screen.getByRole("button", { name: "録音を開始" }));
    await waitFor(async () => expect((await backend.getRecordingStatus()).state).toBe("recording"));
    act(() => backend.setWindowVisible(false));
    expect(await backend.getAppVersion()).toBe("0.2.0");
    expect((await backend.getRecordingStatus()).state).toBe("recording");

    await act(async () => void (await backend.stopRecording()));
    expect(await backend.getAppVersion()).toBe("0.3.0");
  });

  it("[UPD-026] 設定でオフにすると、待っている自動の更新をやめて、知らせるだけに戻る", async () => {
    const user = userEvent.setup();
    const backend = autoUpdateBackend();
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    act(() => backend.simulateUpdate("0.3.0", { announce: true }));
    const banner = await screen.findByRole("region", { name: "アプリの更新" });
    await waitFor(() => expect(banner).toHaveTextContent(READY_TEXT));

    await user.click(screen.getByRole("button", { name: "設定" }));
    const section = await about();
    await user.click(within(section).getByLabelText(/新しいバージョンを自動でインストールする/));
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(() => expect(banner).toHaveTextContent("新しいバージョン 0.3.0 があります"));
    expect(within(banner).getByRole("button", { name: "更新" })).toBeInTheDocument();
    expect(within(section).getByRole("button", { name: "更新して再起動" })).toBeInTheDocument();

    // ウィンドウを閉じても更新しない
    act(() => backend.setWindowVisible(false));
    act(() => backend.setWindowVisible(true));
    expect(await backend.getAppVersion()).toBe("0.2.0");

    // オンに戻すと、またダウンロードして待つ
    await user.click(within(section).getByLabelText(/新しいバージョンを自動でインストールする/));
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(() => expect(banner).toHaveTextContent(READY_TEXT));
  });
});

describe("更新を始められないとき・失敗したとき", () => {
  it("[UPD-008] AI が議事録や回答を作成している間は更新を始めない", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend({
      seed: true,
      version: "0.2.0",
      confirm: () => true,
      streamDelayMs: 30,
      updateStepDelayMs: 5,
    });
    await backend.setApiKey("anthropic", "sk-ant-test");
    backend.simulateUpdate("0.3.0", { announce: true });
    renderWith(backend, <App />);
    await screen.findByDisplayValue("プロダクト定例");
    const noteId = (await backend.listNotes())[0].id;

    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    await screen.findByRole("region", { name: "議事録" });
    const banner = screen.getByRole("region", { name: "アプリの更新" });
    await user.click(within(banner).getByRole("button", { name: "更新" }));

    expect(await within(banner).findByRole("alert")).toHaveTextContent(
      "AI が議事録や回答を作成しています。終わってから更新してください",
    );
    expect((await backend.getUpdateStatus()).phase).toBe("idle");
    expect(await backend.getAppVersion()).toBe("0.2.0");
    // 知らせは残り、あとで更新できる
    expect(within(banner).getByRole("button", { name: "更新" })).toBeInTheDocument();
    await backend.cancelAi(noteId, "enhance");
    await waitFor(() => expect(screen.queryByRole("region", { name: "議事録" })).not.toBeInTheDocument());
  });

  it("[UPD-015] 更新に失敗したら理由を知らせ、新しいバージョンの知らせは残して、もう一度試せる", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    backend.simulateUpdate("0.3.0", { announce: true });
    const install = vi
      .spyOn(backend, "installUpdate")
      .mockRejectedValueOnce("更新をダウンロードできませんでした。インターネット接続を確認して、もう一度お試しください");
    renderWith(backend, <App />);
    const banner = await screen.findByRole("region", { name: "アプリの更新" });

    await user.click(await within(banner).findByRole("button", { name: "更新" }));
    expect(await within(banner).findByRole("alert")).toHaveTextContent(
      "更新をダウンロードできませんでした。インターネット接続を確認して、もう一度お試しください",
    );
    expect(within(banner).getByText(/新しいバージョン 0\.3\.0/)).toBeInTheDocument();

    await user.click(within(banner).getByRole("button", { name: "更新" }));
    expect(install).toHaveBeenCalledTimes(2);
    await waitFor(async () => expect(await backend.getAppVersion()).toBe("0.3.0"), { timeout: 3000 });
  });

  it("[UPD-015] 失敗の知らせは、次に更新を始めたとき（設定画面から始めたときも）に消える", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    backend.simulateUpdate("0.3.0", { announce: true });
    vi.spyOn(backend, "installUpdate").mockRejectedValueOnce(
      "更新をダウンロードできませんでした。インターネット接続を確認して、もう一度お試しください",
    );
    renderWith(backend, <App />);
    const banner = await screen.findByRole("region", { name: "アプリの更新" });
    await user.click(await within(banner).findByRole("button", { name: "更新" }));
    expect(await within(banner).findByRole("alert")).toHaveTextContent("更新をダウンロードできませんでした");

    // 設定画面の［更新して再起動］から、もう一度始める
    await user.click(screen.getByRole("button", { name: "設定" }));
    const section = await about();
    await user.click(await within(section).findByRole("button", { name: "更新して再起動" }));

    // サイドバーの知らせは、ダウンロードの表示に変わり、前の失敗の文は残らない
    expect(await within(banner).findByRole("progressbar", { name: "更新のダウンロード" })).toBeInTheDocument();
    expect(within(banner).queryByRole("alert")).not.toBeInTheDocument();
    expect(within(banner).queryByText(/更新をダウンロードできませんでした/)).not.toBeInTheDocument();
    await waitFor(async () => expect(await backend.getAppVersion()).toBe("0.3.0"), { timeout: 3000 });
  });

  it("[UPD-015] 設定画面でも、失敗の理由（署名を確認できなかった、など）を表示する", async () => {
    const user = userEvent.setup();
    const backend = updateBackend();
    backend.simulateUpdate("0.3.0", { announce: true });
    vi.spyOn(backend, "installUpdate").mockRejectedValueOnce(
      "ダウンロードしたファイルの署名を確認できなかったため、更新を中止しました",
    );
    renderWith(backend, <SettingsDialog onClose={() => {}} />);
    const section = await about();

    await user.click(await within(section).findByRole("button", { name: "更新して再起動" }));

    expect(
      await within(section).findByText("ダウンロードしたファイルの署名を確認できなかったため、更新を中止しました"),
    ).toBeInTheDocument();
    expect(within(section).getByRole("button", { name: "更新して再起動" })).toBeInTheDocument();
    expect(await backend.getAppVersion()).toBe("0.2.0");
  });
});
