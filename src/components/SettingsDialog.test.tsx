import { describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SettingsDialog } from "./SettingsDialog";
import { BackendContext } from "../lib/backend";
import { createMemoryBackend } from "../lib/memoryBackend";

function setup() {
  const backend = createMemoryBackend();
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

  it("Esc で閉じる", async () => {
    const user = userEvent.setup();
    const { onClose } = setup();
    await screen.findByText("API キー");
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalled();
  });
});
