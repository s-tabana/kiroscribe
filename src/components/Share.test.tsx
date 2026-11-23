import { describe, expect, it, vi } from "vitest";
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
  it("[SHR-001][SHR-007][SHR-008] 議事録がないときはメモを書き出し、文字起こしは選んだときだけ含める", async () => {
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

  it("[SHR-001] 議事録があれば既定は議事録だけを書き出す", async () => {
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

  it("[SHR-002][SHR-003] 何も選ばなければ書き出せず、Esc で閉じる", async () => {
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

describe("共有: 閉じ方・保存の失敗・未保存の入力", () => {
  it("[SHR-003] メニューの外を押すと閉じる（メニューの中を押しても閉じない）", async () => {
    const user = userEvent.setup();
    await setup();
    await user.click(screen.getByRole("button", { name: "共有" }));
    const dialog = screen.getByRole("dialog", { name: "共有" });

    await user.click(within(dialog).getByText("含める内容"));
    expect(screen.getByRole("dialog", { name: "共有" })).toBeInTheDocument();

    await user.click(screen.getByLabelText("タイトル"));
    expect(screen.queryByRole("dialog", { name: "共有" })).not.toBeInTheDocument();
  });

  it("[SHR-008] 保存先のダイアログを取り消したら、何も表示しない", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    vi.spyOn(backend, "saveMarkdown").mockResolvedValue(null);
    await user.click(screen.getByRole("button", { name: "共有" }));
    const dialog = screen.getByRole("dialog", { name: "共有" });

    await user.click(within(dialog).getByRole("button", { name: "ファイルに保存…" }));

    await waitFor(() => expect(within(dialog).getByRole("button", { name: "ファイルに保存…" })).toBeEnabled());
    expect(within(dialog).queryByRole("status")).not.toBeInTheDocument();
    expect(backend.lastSaved).toBeNull();
  });

  it("[SHR-010] ファイルに書けなかったら、理由を共有のメニューの中に表示する", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    vi.spyOn(backend, "saveMarkdown").mockRejectedValue("ファイルを保存できませんでした: アクセスが拒否されました。 (os error 5)");
    await user.click(screen.getByRole("button", { name: "共有" }));
    const dialog = screen.getByRole("dialog", { name: "共有" });

    await user.click(within(dialog).getByRole("button", { name: "ファイルに保存…" }));

    const message = await within(dialog).findByRole("status");
    expect(message).toHaveTextContent("ファイルを保存できませんでした: アクセスが拒否されました。 (os error 5)");
    expect(message).toHaveClass("error-text");
    // 画面上部の知らせには出さない
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    // もう一度試せる
    expect(within(dialog).getByRole("button", { name: "ファイルに保存…" })).toBeEnabled();
  });

  it("[SHR-012] 書き出す前に、まだ保存されていない入力を保存して、最新の内容を書き出す", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const noteId = (await backend.listNotes())[0].id;
    const update = vi.spyOn(backend, "updateNote");
    const exportMarkdown = vi.spyOn(backend, "exportMarkdown");

    // タイトルを書き換えて、自動保存（0.5 秒後）を待たずに書き出す
    await user.type(screen.getByLabelText("タイトル"), "（改）");
    await user.click(screen.getByRole("button", { name: "共有" }));
    const dialog = screen.getByRole("dialog", { name: "共有" });
    await user.click(within(dialog).getByRole("button", { name: "Markdown をコピー" }));

    expect(await within(dialog).findByRole("status")).toHaveTextContent("コピーしました");
    expect(update).toHaveBeenCalledWith(noteId, { title: "プロダクト定例（改）" });
    expect(update.mock.invocationCallOrder[0]).toBeLessThan(exportMarkdown.mock.invocationCallOrder[0]);
    expect(backend.lastCopied).toContain("# プロダクト定例（改）");
  });
});
