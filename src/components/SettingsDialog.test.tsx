import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  DEBUG_LOG_SUBMIT_CONFIRM,
  DEBUG_LOG_SUBMIT_OFFLINE,
  SettingsDialog,
  meetingRecordFileName,
} from "./SettingsDialog";
import { BackendContext } from "../lib/backend";
import { createMemoryBackend, type MemoryBackend } from "../lib/memoryBackend";
import { DEFAULT_SETTINGS } from "../lib/types";
import type { LogSubmission, Settings } from "../lib/types";

function setup(settings: Partial<Settings> = {}) {
  const backend = createMemoryBackend({ settings });
  const setApiKey = vi.spyOn(backend, "setApiKey");
  const saveSettings = vi.spyOn(backend, "saveSettings");
  const onClose = vi.fn();
  render(
    <BackendContext.Provider value={backend}>
      <SettingsDialog onClose={onClose} />
    </BackendContext.Provider>,
  );
  return { backend, setApiKey, saveSettings, onClose };
}

describe("SettingsDialog", () => {
  it("[KEY-001][KEY-002][SEC-001][SEC-003] API キーを保存すると入力欄を空にし、状態だけを表示する", async () => {
    const user = userEvent.setup();
    const { setApiKey } = setup();

    const input = await screen.findByLabelText("Anthropic（Claude） の API キー");
    const field = input.closest(".api-key") as HTMLElement;
    expect(within(field).getByText("未設定")).toBeInTheDocument();

    await user.type(input, "sk-ant-secret");
    await user.click(within(field).getByRole("button", { name: "保存" }));

    expect(setApiKey).toHaveBeenCalledWith("anthropic", "sk-ant-secret");
    expect(await within(field).findByText("設定済み")).toBeInTheDocument();
    expect(input).toHaveValue("");
    expect(screen.queryByDisplayValue("sk-ant-secret")).not.toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Anthropic（Claude） のキーを保存しました");

    await user.click(within(field).getByRole("button", { name: "削除" }));
    expect(setApiKey).toHaveBeenLastCalledWith("anthropic", "");
    expect(await within(field).findByText("未設定")).toBeInTheDocument();
  });

  it("[SET-002][SET-012] 設定を変更して保存できる", async () => {
    const user = userEvent.setup();
    const { saveSettings, backend } = setup();

    const name = await screen.findByLabelText("議事録での自分の呼び名");
    await user.type(name, "田花");
    await user.click(screen.getByLabelText(/相手の声/));
    await user.click(screen.getByRole("button", { name: "設定を保存" }));

    expect(saveSettings).toHaveBeenCalledOnce();
    const saved = await backend.getSettings();
    expect(saved.myName).toBe("田花");
    expect(saved.captureSystemAudio).toBe(false);
    expect(await screen.findByText("設定を保存しました")).toBeInTheDocument();
  });

  it("[SET-009] AI のモデルを 4 つの中から選んで保存できる", async () => {
    const user = userEvent.setup();
    const { backend } = setup();

    const select = await screen.findByLabelText("Claude のモデル");
    expect(select).toHaveValue("claude-opus-5-5");
    const options = within(select).getAllByRole("option") as HTMLOptionElement[];
    expect(options.map((option) => [option.value, option.textContent])).toEqual([
      ["claude-opus-5-5", "Claude Opus 5.5（標準）"],
      ["claude-sonnet-5-5", "Claude Sonnet 5.5（速くて安い）"],
      ["claude-haiku-5-5", "Claude Haiku 5.5（最も速くて安い）"],
      ["claude-fable-5-2", "Claude Fable 5.2（最も高性能・高価）"],
    ]);

    // 値が入っていても、ほかのモデルを選べる
    for (const model of ["claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-2"]) {
      await user.selectOptions(select, model);
      await user.click(screen.getByRole("button", { name: "設定を保存" }));
      await waitFor(async () => expect((await backend.getSettings()).claudeModel).toBe(model));
    }
    expect(select).toHaveValue("claude-fable-5-2");
  });

  it("[SET-008] 文字起こしのモデルも一覧から選んで保存できる", async () => {
    const user = userEvent.setup();
    const { backend } = setup();

    const select = await screen.findByLabelText("モデル");
    expect(select).toHaveValue("gpt-4o-transcribe");
    const options = within(select).getAllByRole("option") as HTMLOptionElement[];
    expect(options.map((option) => option.value)).toEqual([
      "gpt-4o-transcribe",
      "gpt-4o-mini-transcribe",
      "whisper-1",
    ]);

    await user.selectOptions(select, "gpt-4o-mini-transcribe");
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(async () => expect((await backend.getSettings()).transcriptionModel).toBe("gpt-4o-mini-transcribe"));
  });

  it("[SET-008][SET-009] 一覧にない保存済みのモデルは、そのまま選択肢に出して保持する", async () => {
    const user = userEvent.setup();
    const { backend, saveSettings } = setup({ claudeModel: "claude-opus-4-8", transcriptionModel: "my-transcriber" });

    const claude = await screen.findByLabelText("Claude のモデル");
    expect(claude).toHaveValue("claude-opus-4-8");
    const options = within(claude).getAllByRole("option") as HTMLOptionElement[];
    expect(options).toHaveLength(5);
    expect(options[4]).toHaveValue("claude-opus-4-8");
    expect(options[4]).toHaveTextContent("claude-opus-4-8");
    expect(screen.getByLabelText("モデル")).toHaveValue("my-transcriber");

    // ほかの設定を変えて保存しても、別のモデルに置き換わらない
    await user.type(screen.getByLabelText("議事録での自分の呼び名"), "田花");
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(() => expect(saveSettings).toHaveBeenCalledOnce());
    const saved = await backend.getSettings();
    expect(saved.claudeModel).toBe("claude-opus-4-8");
    expect(saved.transcriptionModel).toBe("my-transcriber");
    expect(saved.myName).toBe("田花");

    // 一覧のモデルに選び直すと、一覧にない値は選択肢から消える
    await user.selectOptions(claude, "claude-sonnet-5-5");
    expect(within(claude).getAllByRole("option")).toHaveLength(4);
  });

  it("[KEY-009] ［キーを発行する］で、それぞれの発行ページを外部ブラウザで開く", async () => {
    const user = userEvent.setup();
    const { backend } = setup();
    const openExternal = vi.spyOn(backend, "openExternal");

    await user.click(await screen.findByRole("button", { name: "OpenAI のキーを発行する" }));
    expect(openExternal).toHaveBeenLastCalledWith("https://platform.openai.com/api-keys");
    await user.click(screen.getByRole("button", { name: "Anthropic（Claude） のキーを発行する" }));
    expect(openExternal).toHaveBeenLastCalledWith("https://platform.claude.com/settings/keys");
    expect(openExternal).toHaveBeenCalledTimes(2);
    expect(backend.openedUrls).toEqual([
      "https://platform.openai.com/api-keys",
      "https://platform.claude.com/settings/keys",
    ]);
    expect(screen.getAllByRole("button", { name: /のキーを発行する$/ })).toHaveLength(2);
  });

  it("[UPD-019] 自動更新は既定でオフで、オンにして保存できる", async () => {
    const user = userEvent.setup();
    const { saveSettings, backend } = setup();

    const auto = await screen.findByLabelText(/新しいバージョンを自動でインストールする/);
    expect(auto).not.toBeChecked();
    expect(auto.closest("section")).toHaveAccessibleName("このアプリについて");
    expect(screen.getByText(/オフのときは、新しいバージョンがあることだけを知らせます/)).toBeInTheDocument();

    await user.click(auto);
    expect(auto).toBeChecked();
    // ［設定を保存］を押すまでは保存しない
    expect((await backend.getSettings()).autoUpdate).toBe(false);
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ autoUpdate: true }));
    expect(await screen.findByText("設定を保存しました")).toBeInTheDocument();
    expect((await backend.getSettings()).autoUpdate).toBe(true);

    // オフに戻して保存できる
    await user.click(auto);
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(async () => expect((await backend.getSettings()).autoUpdate).toBe(false));
  });

  it("[UPD-019] 保存済みの設定がオンなら、チェックが入った状態で開く", async () => {
    setup({ autoUpdate: true });
    expect(await screen.findByLabelText(/新しいバージョンを自動でインストールする/)).toBeChecked();
  });

  it("[SET-010] Esc で閉じる", async () => {
    const user = userEvent.setup();
    const { onClose } = setup();
    await screen.findByText("API キー");
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalled();
  });
});

describe("SettingsDialog: 読み込み・閉じ方・マイクと言語", () => {
  /** 用意したバックエンドで設定画面を開く（描画の前に、応答を差し替えたいとき用） */
  function open(backend: MemoryBackend) {
    const onClose = vi.fn();
    render(
      <BackendContext.Provider value={backend}>
        <SettingsDialog onClose={onClose} />
      </BackendContext.Provider>,
    );
    return { backend, onClose };
  }

  it("[SET-002] 保存せずに閉じると、変えた内容は捨てる", async () => {
    const user = userEvent.setup();
    const { backend, saveSettings, onClose } = setup();
    await user.type(await screen.findByLabelText("議事録での自分の呼び名"), "田花");
    await user.click(screen.getByLabelText(/相手の声/));
    await user.selectOptions(screen.getByLabelText("言語"), "en");

    await user.click(screen.getAllByRole("button", { name: "閉じる" })[1]);

    expect(onClose).toHaveBeenCalledOnce();
    expect(saveSettings).not.toHaveBeenCalled();
    expect(await backend.getSettings()).toEqual(DEFAULT_SETTINGS);
  });

  it("[SET-006] マイクの欄に、既定のマイクと使えるマイクの一覧を表示し、選んで保存できる", async () => {
    const user = userEvent.setup();
    const { backend } = setup();
    const mic = await screen.findByLabelText("マイク");
    await waitFor(() => expect(within(mic).getAllByRole("option")).toHaveLength(3));
    expect(within(mic).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "既定のマイク（マイク (Realtek High Definition Audio)）",
      "マイク (Realtek High Definition Audio)",
      "ヘッドセット マイク (USB Audio)",
    ]);
    expect(mic).toHaveValue("");

    await user.selectOptions(mic, "ヘッドセット マイク (USB Audio)");
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(async () => expect((await backend.getSettings()).inputDeviceId).toBe("wasapi:headset"));

    // 既定のマイクに戻せる
    await user.selectOptions(mic, "");
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(async () => expect((await backend.getSettings()).inputDeviceId).toBeNull());
  });

  it("[SET-006] 保存してあるマイクが見つからないときは「（見つからないデバイス）」として選んだままにする", async () => {
    const user = userEvent.setup();
    const { backend } = setup({ inputDeviceId: "wasapi:unplugged" });
    const mic = await screen.findByLabelText("マイク");
    await waitFor(() => expect(within(mic).getAllByRole("option")).toHaveLength(4));
    expect(mic).toHaveValue("wasapi:unplugged");
    expect(within(mic).getByRole("option", { name: "（見つからないデバイス）" })).toHaveValue("wasapi:unplugged");

    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await screen.findByText("設定を保存しました");
    expect((await backend.getSettings()).inputDeviceId).toBe("wasapi:unplugged");
  });

  it("[SET-006] マイクの一覧を取得できなかったら、欄の下に理由を表示する", async () => {
    const backend = createMemoryBackend();
    vi.spyOn(backend, "listAudioDevices").mockRejectedValue("音声デバイスのエラー: 入力デバイスを列挙できません");
    open(backend);
    const mic = await screen.findByLabelText(/^マイク/);
    const field = mic.closest("label") as HTMLElement;
    expect(await within(field).findByText("音声デバイスのエラー: 入力デバイスを列挙できません")).toBeInTheDocument();
    expect(within(mic).getAllByRole("option").map((option) => option.textContent)).toEqual(["既定のマイク"]);
  });

  it("[SET-007] 言語は、日本語・English・自動判定から選べる", async () => {
    const user = userEvent.setup();
    const { backend } = setup();
    const language = await screen.findByLabelText("言語");
    expect(within(language).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "日本語",
      "English",
      "自動判定",
    ]);
    expect(language).toHaveValue("ja");

    await user.selectOptions(language, "自動判定");
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(async () => expect((await backend.getSettings()).language).toBe("auto"));
  });

  it("[SET-010] 画面の外側のクリック・「閉じる」・× で閉じる（画面の中のクリックでは閉じない）", async () => {
    const user = userEvent.setup();
    const { onClose } = setup();
    const dialog = await screen.findByRole("dialog", { name: "設定" });

    fireEvent.mouseDown(dialog);
    fireEvent.mouseDown(within(dialog).getByRole("heading", { name: "API キー" }));
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.mouseDown(dialog.parentElement as HTMLElement);
    expect(onClose).toHaveBeenCalledTimes(1);
    const [cross, close] = screen.getAllByRole("button", { name: "閉じる" });
    await user.click(cross);
    await user.click(close);
    expect(onClose).toHaveBeenCalledTimes(3);
  });

  it("[SET-013] 設定や API キーの状態を読み込めなかったら、下部に理由を表示する", async () => {
    const backend = createMemoryBackend();
    vi.spyOn(backend, "getSettings").mockRejectedValue("データベースエラー: database is locked");
    open(backend);
    expect(await screen.findByRole("status")).toHaveTextContent("データベースエラー: database is locked");
    expect(screen.getByRole("status")).toHaveClass("error-text");
    // 読み込めていない設定は保存できない
    expect(screen.getByRole("button", { name: "設定を保存" })).toBeDisabled();
  });
});

describe("SettingsDialog: API キーの欄", () => {
  async function openWithKey() {
    const backend = createMemoryBackend();
    await backend.setApiKey("openai", "sk-saved-openai-key");
    const setApiKey = vi.spyOn(backend, "setApiKey");
    render(
      <BackendContext.Provider value={backend}>
        <SettingsDialog onClose={() => {}} />
      </BackendContext.Provider>,
    );
    const input = await screen.findByLabelText("OpenAI の API キー");
    const field = input.closest(".api-key") as HTMLElement;
    await within(field).findByText("設定済み");
    return { backend, setApiKey, input, field };
  }

  it("[KEY-003] 設定済みのキーは新しいキーで置き換えられ、入力が空白だけのときは保存を押せない", async () => {
    const user = userEvent.setup();
    const { setApiKey, input, field } = await openWithKey();
    const save = within(field).getByRole("button", { name: "保存" });
    expect(input).toHaveAttribute("placeholder", "新しいキーで置き換える");
    expect(save).toBeDisabled();

    await user.type(input, "   ");
    expect(save).toBeDisabled();
    await user.type(input, "{Enter}");
    expect(setApiKey).not.toHaveBeenCalled();

    await user.clear(input);
    await user.type(input, "sk-new-openai-key");
    await user.click(save);
    expect(setApiKey).toHaveBeenCalledExactlyOnceWith("openai", "sk-new-openai-key");
    await waitFor(() => expect(input).toHaveValue(""));
    expect(within(field).getByText("設定済み")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("OpenAI のキーを保存しました");
  });

  it("[KEY-007] キーを保存できなかったら、下部に理由を表示し、状態は変えない", async () => {
    const user = userEvent.setup();
    const { setApiKey } = setup();
    setApiKey.mockRejectedValue("資格情報ストアのエラー: アクセスが拒否されました");
    const input = await screen.findByLabelText("OpenAI の API キー");
    const field = input.closest(".api-key") as HTMLElement;

    await user.type(input, "sk-openai-key");
    await user.click(within(field).getByRole("button", { name: "保存" }));

    const status = await screen.findByRole("status");
    expect(status).toHaveTextContent("資格情報ストアのエラー: アクセスが拒否されました");
    expect(status).toHaveClass("error-text");
    expect(within(field).getByText("未設定")).toBeInTheDocument();
  });

  it("[SEC-003] API キーの入力欄は伏せ字で、保存済みのキーは欄に表示しない", async () => {
    const { input } = await openWithKey();
    expect(input).toHaveAttribute("type", "password");
    expect(screen.getByLabelText("Anthropic（Claude） の API キー")).toHaveAttribute("type", "password");
    // 保存済みのキーは、どの欄にも、画面の文にも出ていない
    expect(input).toHaveValue("");
    expect(screen.queryByDisplayValue(/sk-saved/)).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain("sk-saved");
  });
});

describe("SettingsDialog: 会議の検出の記録", () => {
  it("[MTG-026] ［会議の検出の記録を保存…］で、保存ダイアログから記録を保存し、件数と保存した場所を表示する", async () => {
    const user = userEvent.setup();
    const { backend } = setup();
    const saveRecord = vi
      .spyOn(backend, "saveMeetingRecord")
      .mockResolvedValue({ path: "C:\\Users\\me\\record.json", entries: 12 });

    await user.click(await screen.findByRole("button", { name: "会議の検出の記録を保存…" }));

    expect(saveRecord).toHaveBeenCalledWith(expect.stringMatching(/^kiroscribe-meeting-record-\d{8}-\d{4}\.json$/));
    expect(await screen.findByText("会議の検出の記録を保存しました（12 件）: C:\\Users\\me\\record.json")).toBeInTheDocument();
    // 何が入るかを、押す前に読める
    expect(screen.getByText(/マイクを使ったアプリの名前と、マイクを使っているブラウザのタブの名前だけ/)).toBeInTheDocument();
    // タブの名前に、会議でないページやメールの件名が入りうることと、送る前に確かめることを添える
    expect(screen.getByText(/会議でないページやメールの件名も入ることがあります。人に送る前に、見せたくない内容がないか確かめてください。/)).toBeInTheDocument();
  });

  it("[MTG-026] 保存ダイアログを取り消したら何も表示せず、保存できなかったら理由を表示する", async () => {
    const user = userEvent.setup();
    const { backend } = setup();
    const saveRecord = vi.spyOn(backend, "saveMeetingRecord").mockResolvedValue(null);
    const button = await screen.findByRole("button", { name: "会議の検出の記録を保存…" });

    await user.click(button);
    await waitFor(() => expect(saveRecord).toHaveBeenCalledOnce());
    expect(screen.queryByText(/会議の検出の記録を保存しました/)).not.toBeInTheDocument();

    saveRecord.mockRejectedValue("ファイルを保存できませんでした: アクセスが拒否されました。 (os error 5)");
    await user.click(button);
    expect(await screen.findByText("ファイルを保存できませんでした: アクセスが拒否されました。 (os error 5)")).toBeInTheDocument();
  });

  it("[MTG-026] 記録のファイル名には、保存した日付と時刻を付ける", () => {
    expect(meetingRecordFileName(new Date(2026, 10, 23, 9, 4))).toBe("kiroscribe-meeting-record-20261123-0904.json");
  });
});

describe("SettingsDialog: デバッグ用のログの提出", () => {
  function setupSubmit(
    submitLogs?: (progress: (value: { sent: number; total: number }) => void) => Promise<LogSubmission>,
  ) {
    const confirm = vi.fn(() => true);
    const backend = createMemoryBackend({ confirm, submitLogs });
    const submit = vi.spyOn(backend, "submitDebugLogs");
    render(
      <BackendContext.Provider value={backend}>
        <SettingsDialog onClose={() => undefined} />
      </BackendContext.Provider>,
    );
    return { backend, confirm, submit };
  }

  it("[SET-014] 押すと、送るもの・送らないもの・置く期間を確認し、取り消したら送らない", async () => {
    const user = userEvent.setup();
    const { confirm, submit } = setupSubmit();
    confirm.mockReturnValue(false);

    await user.click(await screen.findByRole("button", { name: "デバッグ用のログを提出…" }));

    expect(confirm).toHaveBeenCalledWith(DEBUG_LOG_SUBMIT_CONFIRM);
    expect(DEBUG_LOG_SUBMIT_CONFIRM).toContain("送るもの: アプリの動きの記録");
    expect(DEBUG_LOG_SUBMIT_CONFIRM).toContain("送らないもの: 録音、ノート・文字起こし・議事録・チャットの内容、API キー");
    expect(DEBUG_LOG_SUBMIT_CONFIRM).toContain("30 日で消します");
    expect(submit).not.toHaveBeenCalled();
    expect(screen.queryByText(/ログを提出しました/)).not.toBeInTheDocument();
  });

  it("[SET-015] 送っている間は進み具合を出してボタンを押せなくし、終わったら提出の番号を出す", async () => {
    const user = userEvent.setup();
    let report: (value: { sent: number; total: number }) => void = () => undefined;
    let finish: (submission: LogSubmission) => void = () => undefined;
    const { backend, submit } = setupSubmit(
      (progress) =>
        new Promise((resolve) => {
          report = progress;
          finish = resolve;
        }),
    );
    const copy = vi.spyOn(backend, "copyText");
    const button = await screen.findByRole("button", { name: "デバッグ用のログを提出…" });

    await user.click(button);
    expect(submit).toHaveBeenCalledOnce();
    expect(await screen.findByText("ログをまとめています…")).toBeInTheDocument();
    expect(button).toBeDisabled();

    act(() => report({ sent: 1_000, total: 4_000 }));
    expect(await screen.findByText(/ログを送っています… 25%/)).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "ログの送信" })).toHaveAttribute("value", "25");

    await act(async () => finish({ id: "KS-20261127-7F3A", receivedAt: "2026-11-27T12:50:30.217Z" }));
    expect(await screen.findByText(/ログを提出しました。提出の番号:/)).toHaveTextContent(
      "ログを提出しました。提出の番号: KS-20261127-7F3A（問い合わせるときに伝えてください）",
    );
    expect(button).toBeEnabled();
    expect(screen.queryByText(/ログを送っています/)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "番号をコピー" }));
    expect(copy).toHaveBeenCalledWith("KS-20261127-7F3A");
  });

  it("[SET-016] 送れなかったときは理由を出し、もう一度押せる", async () => {
    const user = userEvent.setup();
    const { submit } = setupSubmit(() =>
      Promise.reject("ログを送れませんでした。送り先につながりませんでした。インターネットに接続しているか、プロキシの設定を確かめてください（接続が拒否されました）"),
    );
    const button = await screen.findByRole("button", { name: "デバッグ用のログを提出…" });

    await user.click(button);
    expect(await screen.findByText(/ログを送れませんでした。送り先につながりませんでした/)).toBeInTheDocument();
    expect(button).toBeEnabled();

    submit.mockResolvedValueOnce({ id: "KS-20261127-AAAA", receivedAt: "2026-11-27T12:50:30.217Z" });
    await user.click(button);
    expect(await screen.findByText(/ログを提出しました。提出の番号:/)).toHaveTextContent("KS-20261127-AAAA");
    expect(screen.queryByText(/ログを送れませんでした/)).not.toBeInTheDocument();
  });

  it("[SET-016] オフラインのときは送らずに、つながっていないことを出す", async () => {
    const user = userEvent.setup();
    const { submit } = setupSubmit();
    const online = vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
    try {
      await user.click(await screen.findByRole("button", { name: "デバッグ用のログを提出…" }));
      expect(await screen.findByText(DEBUG_LOG_SUBMIT_OFFLINE)).toBeInTheDocument();
      expect(submit).not.toHaveBeenCalled();
    } finally {
      online.mockRestore();
    }
  });
});
