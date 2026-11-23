import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SettingsDialog } from "./SettingsDialog";
import { BackendContext } from "../lib/backend";
import { createMemoryBackend } from "../lib/memoryBackend";
import type { Settings } from "../lib/types";

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
  it("API キーを保存すると入力欄を空にし、状態だけを表示する", async () => {
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

  it("設定を変更して保存できる", async () => {
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

  it("AI のモデルを 4 つの中から選んで保存できる", async () => {
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

  it("文字起こしのモデルも一覧から選んで保存できる", async () => {
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

  it("一覧にない保存済みのモデルは、そのまま選択肢に出して保持する", async () => {
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

  it("［キーを発行する］で、それぞれの発行ページを外部ブラウザで開く", async () => {
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

  it("自動更新は既定でオフで、オンにして保存できる", async () => {
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

  it("保存済みの設定がオンなら、チェックが入った状態で開く", async () => {
    setup({ autoUpdate: true });
    expect(await screen.findByLabelText(/新しいバージョンを自動でインストールする/)).toBeChecked();
  });

  it("Esc で閉じる", async () => {
    const user = userEvent.setup();
    const { onClose } = setup();
    await screen.findByText("API キー");
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalled();
  });
});
