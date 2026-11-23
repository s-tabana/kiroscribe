import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../App";
import { BackendContext } from "../lib/backend";
import { createMemoryBackend } from "../lib/memoryBackend";
import type { Note } from "../lib/types";

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
  it("[MIN-001][MIN-011][MIN-012] メモから議事録を作り、自分のメモ由来の行と AI が補った行を色分けする", async () => {
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

  it("[MIN-006] タイトルが空なら生成したタイトルを付ける", async () => {
    const user = userEvent.setup();
    await setup();
    await user.click(screen.getByRole("button", { name: /新しいノート/ }));
    await user.type(await screen.findByLabelText("自分のメモ"), "- 予算の件");
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    await waitFor(() => expect(screen.getByLabelText("タイトル")).toHaveValue("プロダクト定例（リリース確認）"), {
      timeout: 3000,
    });
  });

  it("[MIN-003][ERR-002] Anthropic のキーがないと設定へ案内する", async () => {
    const user = userEvent.setup();
    await setup({ key: false });
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("API キーが設定されていません（Anthropic）");
    expect(within(alert).getByRole("button", { name: "設定を開く" })).toBeInTheDocument();
    // 議事録の表示には切り替わらない
    expect(screen.queryByRole("tab", { name: "議事録" })).not.toBeInTheDocument();
  });

  it("[MIN-009][MIN-010] 作り直しは確認してから行い、手で直した議事録は保存される", async () => {
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

  it("[MIN-005][ERR-009] 生成を中止できる", async () => {
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
  it("[CHT-001][CHT-002][CHT-003][CHT-011][CHT-012] 提案から質問でき、履歴が残り、消去できる", async () => {
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

  it("[CHT-004] キーがないときは質問を入力欄に戻してエラーを知らせる", async () => {
    const user = userEvent.setup();
    await setup({ key: false });
    await user.click(screen.getByRole("button", { name: "AI チャット" }));
    const panel = screen.getByRole("complementary", { name: "AI チャット" });
    await user.type(within(panel).getByLabelText("AI への質問"), "要点は？{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent("Anthropic");
    expect(within(panel).getByLabelText("AI への質問")).toHaveValue("要点は？");
  });
});

describe("AI 議事録: 作成のボタンと作成中の表示", () => {
  it("[MIN-001] 議事録を作成のボタンは、議事録がまだなく、メモか文字起こしがあるノートにだけ出す", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const emptyId = (await backend.listNotes())[2].id;
    const list = screen.getByRole("navigation", { name: "ノート一覧" });

    // メモも文字起こしもないノート
    await user.click(within(list).getByText("無題のノート"));
    await waitFor(() => expect(screen.getByLabelText("タイトル")).toHaveValue(""));
    expect(screen.queryByRole("button", { name: "議事録を作成" })).not.toBeInTheDocument();

    // メモを書くと出る。消すと出なくなる
    const memo = screen.getByLabelText("自分のメモ");
    await user.type(memo, "- 予算の件");
    expect(screen.getByRole("button", { name: "議事録を作成" })).toBeInTheDocument();
    await user.clear(memo);
    expect(screen.queryByRole("button", { name: "議事録を作成" })).not.toBeInTheDocument();

    // 文字起こしだけでも出る
    act(() => {
      backend.emitSegment(emptyId, "them", "予算は来期に増額します");
    });
    expect(await screen.findByRole("button", { name: "議事録を作成" })).toBeInTheDocument();

    // 議事録ができたあとは出ない（作り直すを使う）
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    await waitForMinutes();
    expect(screen.queryByRole("button", { name: "議事録を作成" })).not.toBeInTheDocument();
  });

  it("[MIN-004] 作成中は、読んでいる表示から、届いた文章を順に見せる表示に変わる（タイトル行は見せない）", async () => {
    const user = userEvent.setup();
    await setup({ streamDelayMs: 10 });

    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    const minutes = await screen.findByRole("region", { name: "議事録" });
    expect(minutes).toHaveAttribute("aria-busy", "true");
    expect(within(minutes).getByText("メモと文字起こしを読んでいます…")).toBeInTheDocument();

    // 文章が届き始めると、届いた分から表示する
    expect(await within(minutes).findByRole("heading", { name: "概要" })).toBeInTheDocument();
    expect(within(minutes).getByText("議事録を作成しています…")).toBeInTheDocument();
    expect(within(minutes).queryByText("メモと文字起こしを読んでいます…")).not.toBeInTheDocument();
    expect(within(minutes).queryByRole("heading", { name: "ToDo" })).not.toBeInTheDocument();
    // 先頭の「# タイトル」の行は表示しない
    expect(within(minutes).queryByRole("heading", { level: 1 })).not.toBeInTheDocument();

    const done = await waitForMinutes();
    expect(within(done).getByRole("heading", { name: "ToDo" })).toBeInTheDocument();
    expect(within(done).queryByRole("heading", { level: 1 })).not.toBeInTheDocument();
  });

  it("[MIN-009] 作り直しで OK を選ぶと、今の議事録が新しい内容に置き換わる", async () => {
    const user = userEvent.setup();
    const confirm = vi.fn(() => true);
    const backend = await setup({ confirm });
    const noteId = (await backend.listNotes())[0].id;
    await backend.updateNote(noteId, { enhancedNotes: "## 決定事項\n- 手で直した" });
    // 議事録のあるノートとして開き直す
    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    await user.click(within(list).getByText("採用面談（バックエンド）"));
    await screen.findByDisplayValue("採用面談（バックエンド）");
    await user.click(within(list).getByText("プロダクト定例"));
    const before = await screen.findByRole("region", { name: "議事録" });
    expect(within(before).getByText("手で直した")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "作り直す" }));

    expect(confirm).toHaveBeenCalledOnce();
    expect(confirm).toHaveBeenCalledWith(
      "議事録を作り直しますか？\n今の議事録（手で直した部分も含む）は新しい内容に置き換わります。",
    );
    // 作成が終わって（作成中の表示ではなくなって）、新しい内容になっている
    await waitFor(
      () => {
        const minutes = screen.getByRole("region", { name: "議事録" });
        expect(minutes).toHaveAttribute("aria-busy", "false");
        expect(within(minutes).getByText("リリース日は 12月5日で確定。")).toBeInTheDocument();
      },
      { timeout: 3000 },
    );
    expect(screen.queryByText("手で直した")).not.toBeInTheDocument();
    expect((await backend.getNote(noteId)).enhancedNotes).not.toContain("手で直した");
  });

  it("[MIN-017] 作成に失敗したら理由を知らせ、すでにある議事録はそのまま残す", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const noteId = (await backend.listNotes())[0].id;
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    await waitForMinutes();
    const saved = (await backend.getNote(noteId)).enhancedNotes;

    vi.spyOn(backend, "enhanceNote").mockRejectedValueOnce("Anthropic がエラーを返しました（HTTP 500）: Internal server error");
    await user.click(screen.getByRole("button", { name: "作り直す" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Anthropic がエラーを返しました（HTTP 500）: Internal server error");
    const minutes = screen.getByRole("region", { name: "議事録" });
    await waitFor(() => expect(minutes).toHaveAttribute("aria-busy", "false"));
    expect(within(minutes).getByText("リリース日は 12月5日で確定。")).toBeInTheDocument();
    expect((await backend.getNote(noteId)).enhancedNotes).toBe(saved);
  });

  it("[MIN-017] 議事録がまだないノートで失敗したら、何も保存せず、自分のメモの表示に戻る", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    vi.spyOn(backend, "enhanceNote").mockRejectedValueOnce("通信に失敗しました（connection reset）");

    await user.click(screen.getByRole("button", { name: "議事録を作成" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("通信に失敗しました（connection reset）");
    await waitFor(() => expect(screen.queryByRole("region", { name: "議事録" })).not.toBeInTheDocument());
    expect(screen.queryByRole("tab", { name: "議事録" })).not.toBeInTheDocument();
    expect(screen.getByLabelText("自分のメモ")).toBeInTheDocument();
    expect((await backend.listNotes())[0].hasEnhanced).toBe(false);
    // もう一度始められる
    expect(screen.getByRole("button", { name: "議事録を作成" })).toBeEnabled();
  });
});

describe("AI 議事録: 作成中にノートを切り替えたとき", () => {
  async function openNote(user: ReturnType<typeof userEvent.setup>, title: string) {
    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    await user.click(within(list).getByText(title));
    await screen.findByDisplayValue(title);
  }

  it("[MIN-019] 作成中に別のノートへ移って戻ると、届いた文章とともに作成中の表示に戻り、終わると完成した議事録になる", async () => {
    const user = userEvent.setup();
    const backend = await setup({ streamDelayMs: 25 });
    const noteId = (await backend.listNotes())[0].id;

    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    await within(await screen.findByRole("region", { name: "議事録" })).findByRole("heading", { name: "概要" });

    await openNote(user, "採用面談（バックエンド）");
    // 別のノートは、作成中の表示にならない
    expect(screen.queryByRole("region", { name: "議事録" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "議事録を作成" })).toBeEnabled();

    await openNote(user, "プロダクト定例");
    const minutes = await screen.findByRole("region", { name: "議事録" });
    expect(minutes).toHaveAttribute("aria-busy", "true");
    expect(within(minutes).getByText("議事録を作成しています…")).toBeInTheDocument();
    // 離れる前に届いていた文章も表示している
    expect(within(minutes).getByRole("heading", { name: "概要" })).toBeInTheDocument();
    expect(within(minutes).getByRole("button", { name: "中止" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "議事録" })).toHaveAttribute("aria-selected", "true");

    // 終わると、完成した議事録に切り替わる
    const done = await waitForMinutes();
    expect(done).toHaveAttribute("aria-busy", "false");
    expect(within(done).getByText("リリース日は 12月5日で確定。")).toBeInTheDocument();
    expect(within(done).getByRole("heading", { name: "ToDo" })).toBeInTheDocument();
    expect(screen.queryByText("議事録を作成しています…")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect((await backend.getNote(noteId)).enhancedNotes).toContain("## ToDo");
  });

  it("[MIN-019] 作成は別のノートを開いている間も続いて保存され、終わってから戻ると完成した議事録を表示する", async () => {
    const user = userEvent.setup();
    const backend = await setup({ streamDelayMs: 5 });
    const noteId = (await backend.listNotes())[0].id;

    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    await screen.findByRole("region", { name: "議事録" });
    await openNote(user, "採用面談（バックエンド）");

    await waitFor(async () => expect((await backend.getNote(noteId)).enhancedNotes).not.toBeNull(), { timeout: 5000 });
    // 一覧のプレビューも、議事録のものに変わる
    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    expect(await within(list).findByText("12月5日のリリースに向けた残課題と、価格改定の扱いを確認した。")).toBeInTheDocument();
    expect(screen.getByDisplayValue("採用面談（バックエンド）")).toBeInTheDocument();

    await openNote(user, "プロダクト定例");
    const minutes = await waitForMinutes();
    expect(minutes).toHaveAttribute("aria-busy", "false");
    expect(within(minutes).getByText("リリース日は 12月5日で確定。")).toBeInTheDocument();
  });

  it("[MIN-019] 戻ったあとで作成が失敗したら、知らせを 1 回だけ出して、自分のメモの表示に戻る", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    let fail!: (reason: string) => void;
    vi.spyOn(backend, "enhanceNote").mockReturnValueOnce(
      new Promise<Note>((_, reject) => {
        fail = reject;
      }),
    );

    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    await screen.findByText("メモと文字起こしを読んでいます…");
    await openNote(user, "採用面談（バックエンド）");
    await openNote(user, "プロダクト定例");
    expect(await screen.findByText("メモと文字起こしを読んでいます…")).toBeInTheDocument();

    await act(async () => {
      fail("Anthropic の利用上限に達しました。しばらく待ってから試してください（HTTP 429: rate limited）");
    });

    await waitFor(() => expect(screen.queryByRole("region", { name: "議事録" })).not.toBeInTheDocument());
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(screen.getByRole("alert")).toHaveTextContent("Anthropic の利用上限に達しました");
    expect(screen.getByLabelText("自分のメモ")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "議事録を作成" })).toBeEnabled();
  });
});

describe("AI チャット: 入力と中止", () => {
  async function openChat(opts: Parameters<typeof setup>[0] = {}) {
    const user = userEvent.setup();
    const backend = await setup(opts);
    const noteId = (await backend.listNotes())[0].id;
    const send = vi.spyOn(backend, "sendChatMessage");
    await user.click(screen.getByRole("button", { name: "AI チャット" }));
    const panel = screen.getByRole("complementary", { name: "AI チャット" });
    const input = within(panel).getByLabelText("AI への質問");
    return { user, backend, noteId, send, panel, input };
  }

  it("[CHT-002] Shift+Enter は改行になり、変換を確定する Enter と空白だけの入力では送らない", async () => {
    const { user, noteId, send, panel, input } = await openChat();

    // 空白だけのときは送れない
    expect(within(panel).getByRole("button", { name: "送信" })).toBeDisabled();
    await user.type(input, "   {Enter}");
    expect(within(panel).getByRole("button", { name: "送信" })).toBeDisabled();
    expect(send).not.toHaveBeenCalled();

    await user.clear(input);
    await user.type(input, "決まったことは？{Shift>}{Enter}{/Shift}期限も教えて");
    expect(input).toHaveValue("決まったことは？\n期限も教えて");
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    expect(send).not.toHaveBeenCalled();

    await user.click(within(panel).getByRole("button", { name: "送信" }));
    expect(send).toHaveBeenCalledExactlyOnceWith(noteId, "決まったことは？\n期限も教えて");
    expect(await within(panel).findByText(/リリース日は/, undefined, { timeout: 3000 })).toBeInTheDocument();
  });

  it("[CHT-006] 回答の作成中に中止でき、知らせは出さず、回答は保存しない", async () => {
    const { user, backend, noteId, panel, input } = await openChat({ streamDelayMs: 30 });

    await user.type(input, "決まったことは？{Enter}");
    await user.click(await within(panel).findByRole("button", { name: "中止" }));

    expect(await within(panel).findByRole("button", { name: "送信" })).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    const history = await backend.listChatMessages(noteId);
    expect(history.filter((m) => m.role === "assistant")).toHaveLength(0);
    // 送れなかった質問は入力欄に戻る
    expect(input).toHaveValue("決まったことは？");
  });

  it("[CHT-010] 回答の作成中は、次の質問を送れない", async () => {
    const { user, backend, noteId, send, panel, input } = await openChat({ streamDelayMs: 30 });

    await user.type(input, "決まったことは？{Enter}");
    await within(panel).findByRole("button", { name: "中止" });
    expect(within(panel).queryByRole("button", { name: "送信" })).not.toBeInTheDocument();
    await user.type(input, "自分の ToDo は？{Enter}");

    expect(send).toHaveBeenCalledTimes(1);
    await backend.cancelAi(noteId, "chat");
    await within(panel).findByRole("button", { name: "送信" });
  });

  it("[CHT-012] 質問は書いたままの文字で、回答は Markdown として整えて表示する", async () => {
    const { user, panel, input } = await openChat();

    await user.type(input, "**決まった**ことは？{Enter}");

    const emphasis = await within(panel).findByText("12月5日", undefined, { timeout: 3000 });
    expect(emphasis.tagName).toBe("STRONG");
    expect(within(panel).getByText("**決まった**ことは？")).toBeInTheDocument();
    await waitFor(() => expect(within(panel).getAllByRole("listitem")).toHaveLength(2), { timeout: 3000 });
  });
});

describe("AI 議事録: 編集で空にしたとき", () => {
  it("[MIN-010] 議事録を空（空白だけ）にして編集を終えると、議事録なしに戻り、議事録を作成のボタンが出る", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const noteId = (await backend.listNotes())[0].id;
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    const minutes = await waitForMinutes();

    await user.click(within(minutes).getByRole("button", { name: "編集" }));
    const editor = screen.getByLabelText("議事録の編集");
    await user.clear(editor);
    await user.type(editor, "  {Enter} ");
    // 編集している間は、空でも編集の画面のまま
    expect(screen.getByLabelText("議事録の編集")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "編集を終える" }));

    await waitFor(() => expect(screen.queryByRole("tab", { name: "議事録" })).not.toBeInTheDocument());
    expect(screen.getByLabelText("自分のメモ")).toHaveValue(
      "- リリース日 12/5 で確定？\n- QA の人手が足りない\n- 価格改定は来期に持ち越し",
    );
    expect(screen.getByRole("button", { name: "議事録を作成" })).toBeEnabled();
    await waitFor(async () => expect((await backend.getNote(noteId)).enhancedNotes).toBeNull());
    expect((await backend.getNote(noteId)).enhancedAt).toBeNull();
    expect((await backend.listNotes())[0].hasEnhanced).toBe(false);
    // 一覧のプレビューは、自分のメモのものに戻る
    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    expect(await within(list).findByText("リリース日 12/5 で確定？")).toBeInTheDocument();

    // 開き直しても、議事録なしのまま
    await user.click(within(list).getByText("採用面談（バックエンド）"));
    await screen.findByDisplayValue("採用面談（バックエンド）");
    await user.click(within(list).getByText("プロダクト定例"));
    await screen.findByDisplayValue("プロダクト定例");
    expect(screen.queryByRole("tab", { name: "議事録" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "議事録を作成" })).toBeInTheDocument();
  });

  it("[MIN-010] 編集して中身があれば、0.5 秒後に自動で保存し、議事録ありのまま", async () => {
    const user = userEvent.setup();
    const backend = await setup();
    const noteId = (await backend.listNotes())[0].id;
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));
    const minutes = await waitForMinutes();

    await user.click(within(minutes).getByRole("button", { name: "編集" }));
    const editor = screen.getByLabelText("議事録の編集");
    await user.clear(editor);
    await user.type(editor, "## 決定事項{Enter}- 書き直した");
    // 「編集を終える」を押さなくても保存される
    await waitFor(async () => expect((await backend.getNote(noteId)).enhancedNotes).toBe("## 決定事項\n- 書き直した"), {
      timeout: 3000,
    });
    expect(screen.getByLabelText("議事録の編集")).toBeInTheDocument();
    expect((await backend.listNotes())[0].hasEnhanced).toBe(true);
  });
});
