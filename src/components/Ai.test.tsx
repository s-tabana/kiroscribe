import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { BackendContext } from "../lib/backend";
import { createMemoryBackend } from "../lib/memoryBackend";

/** 生成が終わって議事録のツールバーが出るまで待つ */
async function waitForMinutes() {
  await screen.findByRole("button", { name: "作り直す" }, { timeout: 3000 });
  return screen.getByRole("region", { name: "議事録" });
}

async function setup(opts: { key?: boolean; confirm?: () => boolean; streamDelayMs?: number } = {}) {
  const backend = createMemoryBackend({
    seed: true,
    confirm: opts.confirm ?? (() => true),
    streamDelayMs: opts.streamDelayMs ?? 0,
  });
  if (opts.key ?? true) await backend.setApiKey("anthropic", "sk-ant-test");
  render(
    <BackendContext.Provider value={backend}>
      <App />
    </BackendContext.Provider>,
  );
  await screen.findByDisplayValue("プロダクト定例");
  return backend;
}

describe("AI 議事録", () => {
  it("メモから議事録を作り、自分のメモ由来の行と AI が補った行を色分けする", async () => {
    const user = userEvent.setup();
    const backend = await setup();

    await user.click(screen.getByRole("button", { name: "議事録を作成" }));

    const minutes = await waitForMinutes();
    expect(minutes).toHaveAttribute("aria-busy", "false");
    expect(within(minutes).getByText("リリース日は 12月5日で確定。")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "議事録" })).toHaveAttribute("aria-selected", "true");

    // メモに書いた行は濃く、AI が補った行は薄く
    expect(within(minutes).getByText("リリース日 12/5 で確定？").closest("li")).toHaveClass("src-me");
    expect(within(minutes).getByText("外部 API の回答がなければ、該当機能だけ次のリリースに回す。").closest("li")).toHaveClass("src-ai");

    // 保存されている
    const [first] = await backend.listNotes();
    expect(first.hasEnhanced).toBe(true);
    expect((await backend.getNote(first.id)).enhancedNotes).toContain("## 決定事項");

    // 自分のメモにも戻れる
    await user.click(screen.getByRole("tab", { name: "自分のメモ" }));
    expect(screen.getByLabelText("自分のメモ")).toHaveValue(
      "- リリース日 12/5 で確定？\n- QA の人手が足りない\n- 価格改定は来期に持ち越し",
    );
  });

  it("タイトルが空なら生成したタイトルを付ける", async () => {
    const user = userEvent.setup();
    await setup();
    await user.click(screen.getByRole("button", { name: /新しいノート/ }));
    await user.type(await screen.findByLabelText("自分のメモ"), "- 予算の件");
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    await waitFor(() => expect(screen.getByLabelText("タイトル")).toHaveValue("プロダクト定例（リリース確認）"), {
      timeout: 3000,
    });
  });

  it("Anthropic のキーがないと設定へ案内する", async () => {
    const user = userEvent.setup();
    await setup({ key: false });
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("API キーが設定されていません（Anthropic）");
    expect(within(alert).getByRole("button", { name: "設定を開く" })).toBeInTheDocument();
    // 議事録の表示には切り替わらない
    expect(screen.queryByRole("tab", { name: "議事録" })).not.toBeInTheDocument();
  });

  it("作り直しは確認してから行い、手で直した議事録は保存される", async () => {
    const user = userEvent.setup();
    const confirm = vi.fn(() => false);
    const backend = await setup({ confirm });
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    const minutes = await waitForMinutes();

    // 手で直す
    await user.click(within(minutes).getByRole("button", { name: "編集" }));
    const editor = screen.getByLabelText("議事録の編集");
    await user.clear(editor);
    await user.type(editor, "## 決定事項\n- 手で直した");
    await user.click(screen.getByRole("button", { name: "編集を終える" }));
    expect(await within(minutes).findByText("手で直した")).toBeInTheDocument();
    const noteId = (await backend.listNotes())[0].id;
    await waitFor(async () =>
      expect((await backend.getNote(noteId)).enhancedNotes).toBe("## 決定事項\n- 手で直した"),
    );

    // 作り直しを取り消すと何も変わらない
    const enhance = vi.spyOn(backend, "enhanceNote");
    await user.click(screen.getByRole("button", { name: "作り直す" }));
    expect(confirm).toHaveBeenCalledOnce();
    expect(enhance).not.toHaveBeenCalled();
    expect(within(minutes).getByText("手で直した")).toBeInTheDocument();
  });

  it("生成を中止できる", async () => {
    const user = userEvent.setup();
    const backend = await setup({ streamDelayMs: 20 });
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    const minutes = await screen.findByRole("region", { name: "議事録" });
    expect(minutes).toHaveAttribute("aria-busy", "true");
    await user.click(within(minutes).getByRole("button", { name: "中止" }));
    await waitFor(() => expect(screen.queryByRole("region", { name: "議事録" })).not.toBeInTheDocument());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect((await backend.listNotes())[0].hasEnhanced).toBe(false);
  });
});

describe("AI チャット", () => {
  it("提案から質問でき、履歴が残り、消去できる", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    await user.click(screen.getByRole("button", { name: "AI チャット" }));
    const panel = screen.getByRole("complementary", { name: "AI チャット" });

    await user.click(within(panel).getByRole("button", { name: "この会議で決まったことは？" }));
    expect(await within(panel).findByText(/リリース日は/, undefined, { timeout: 3000 })).toBeInTheDocument();
    const noteId = (await backend.listNotes())[0].id;
    await waitFor(async () => expect(await backend.listChatMessages(noteId)).toHaveLength(2));

    // 入力欄から Enter で送る
    await user.type(within(panel).getByLabelText("AI への質問"), "自分の ToDo は？{Enter}");
    expect(await within(panel).findByText(/ToDo は 1 件/, undefined, { timeout: 3000 })).toBeInTheDocument();
    expect(within(panel).getByLabelText("AI への質問")).toHaveValue("");
    await waitFor(async () => expect(await backend.listChatMessages(noteId)).toHaveLength(4));

    await user.click(await within(panel).findByRole("button", { name: "チャットの履歴を消去" }));
    await waitFor(() => expect(within(panel).getByRole("button", { name: "相手の懸念点は？" })).toBeInTheDocument());
    expect(await backend.listChatMessages(noteId)).toHaveLength(0);
  });

  it("キーがないときは質問を入力欄に戻してエラーを知らせる", async () => {
    const user = userEvent.setup();
    await setup({ key: false });
    await user.click(screen.getByRole("button", { name: "AI チャット" }));
    const panel = screen.getByRole("complementary", { name: "AI チャット" });
    await user.type(within(panel).getByLabelText("AI への質問"), "要点は？{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent("Anthropic");
    expect(within(panel).getByLabelText("AI への質問")).toHaveValue("要点は？");
  });
});
