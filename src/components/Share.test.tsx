import { describe, expect, it } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { BackendContext } from "../lib/backend";
import { createMemoryBackend } from "../lib/memoryBackend";

async function setup() {
  const backend = createMemoryBackend({ seed: true, confirm: () => true, streamDelayMs: 0 });
  await backend.setApiKey("anthropic", "sk-ant-test");
  render(
    <BackendContext.Provider value={backend}>
      <App />
    </BackendContext.Provider>,
  );
  await screen.findByDisplayValue("プロダクト定例");
  return backend;
}

describe("共有（Markdown 書き出し）", () => {
  it("議事録がないときはメモを書き出し、文字起こしは選んだときだけ含める", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const noteId = (await backend.listNotes())[0].id;
    act(() => {
      backend.emitSegment(noteId, "them", "リリースは12月5日でいいですか");
    });

    await user.click(screen.getByRole("button", { name: "共有" }));
    const dialog = screen.getByRole("dialog", { name: "共有" });
    expect(within(dialog).getByLabelText(/議事録/)).toBeDisabled();
    expect(within(dialog).getByLabelText("自分のメモ")).toBeChecked();
    expect(within(dialog).getByLabelText("文字起こし")).not.toBeChecked();

    await user.click(within(dialog).getByRole("button", { name: "Markdown をコピー" }));
    expect(await within(dialog).findByRole("status")).toHaveTextContent("コピーしました");
    expect(backend.lastCopied).toContain("# プロダクト定例");
    expect(backend.lastCopied).toContain("## 自分のメモ\n\n- リリース日 12/5 で確定？");
    expect(backend.lastCopied).not.toContain("## 文字起こし");

    await user.click(within(dialog).getByLabelText("文字起こし"));
    await user.click(within(dialog).getByRole("button", { name: "ファイルに保存…" }));
    expect(await within(dialog).findByRole("status")).toHaveTextContent(/保存しました: \d{4}-\d{2}-\d{2} プロダクト定例\.md/);
    expect(backend.lastSaved?.markdown).toContain("**相手**: リリースは12月5日でいいですか");
  });

  it("議事録があれば既定は議事録だけを書き出す", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    await screen.findByRole("button", { name: "作り直す" }, { timeout: 3000 });

    await user.click(screen.getByRole("button", { name: "共有" }));
    const dialog = screen.getByRole("dialog", { name: "共有" });
    expect(within(dialog).getByLabelText(/議事録/)).toBeChecked();
    expect(within(dialog).getByLabelText("自分のメモ")).not.toBeChecked();

    await user.click(within(dialog).getByRole("button", { name: "Markdown をコピー" }));
    await waitFor(() => expect(backend.lastCopied).toContain("## 決定事項"));
    expect(backend.lastCopied).not.toContain("## 自分のメモ");
  });

  it("何も選ばなければ書き出せず、Esc で閉じる", async () => {
    const user = userEvent.setup();
    await setup();
    await user.click(screen.getByRole("button", { name: "共有" }));
    const dialog = screen.getByRole("dialog", { name: "共有" });
    await user.click(within(dialog).getByLabelText("自分のメモ"));
    expect(within(dialog).getByRole("button", { name: "Markdown をコピー" })).toBeDisabled();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "共有" })).not.toBeInTheDocument();
  });
});
