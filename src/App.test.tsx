import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "./App";
import { BackendContext, type Backend } from "./lib/backend";
import { createMemoryBackend } from "./lib/memoryBackend";

function renderApp(backend: Backend = createMemoryBackend({ confirm: () => true })) {
  render(
    <BackendContext.Provider value={backend}>
      <App />
    </BackendContext.Provider>,
  );
  return backend;
}

describe("App", () => {
  it("[NOTE-003] ノートがないときは作成を促す", async () => {
    renderApp();
    expect(await screen.findByText("最初の会議ノートを作りましょう")).toBeInTheDocument();
  });

  it("[NOTE-001][NOTE-009] 新しいノートを作ってタイトルとメモを書くと、一覧に反映され保存される", async () => {
    const user = userEvent.setup();
    const backend = renderApp();

    await user.click(await screen.findByRole("button", { name: /新しいノート/ }));
    const title = await screen.findByLabelText("タイトル");
    expect(title).toHaveFocus();

    await user.type(title, "週次定例");
    await user.type(screen.getByLabelText("自分のメモ"), "- リリース日を確認");

    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    await waitFor(() => expect(within(list).getByText("週次定例")).toBeInTheDocument(), {
      timeout: 2000,
    });
    expect(within(list).getByText("リリース日を確認")).toBeInTheDocument();

    const [saved] = await backend.listNotes();
    expect(saved.title).toBe("週次定例");
    expect(saved.preview).toBe("リリース日を確認");
  });

  it("[NOTE-008] 検索で一覧を絞り込める", async () => {
    const user = userEvent.setup();
    renderApp(createMemoryBackend({ seed: true }));
    const list = await screen.findByRole("navigation", { name: "ノート一覧" });
    await within(list).findByText("プロダクト定例");

    await user.type(screen.getByLabelText("ノートを検索"), "採用");
    expect(within(list).queryByText("プロダクト定例")).not.toBeInTheDocument();
    expect(within(list).getByText("採用面談（バックエンド）")).toBeInTheDocument();

    await user.clear(screen.getByLabelText("ノートを検索"));
    await user.type(screen.getByLabelText("ノートを検索"), "存在しない");
    expect(within(list).getByText("一致するノートがありません")).toBeInTheDocument();
  });

  it("[NOTE-014][NOTE-020] 確認してからノートを削除し、次のノートを選ぶ", async () => {
    const user = userEvent.setup();
    const confirm = vi.fn(() => true);
    const backend = renderApp(createMemoryBackend({ seed: true, confirm }));
    const list = await screen.findByRole("navigation", { name: "ノート一覧" });
    await within(list).findByText("プロダクト定例");
    expect(await screen.findByDisplayValue("プロダクト定例")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "ノートを削除" }));

    expect(confirm).toHaveBeenCalledOnce();
    await waitFor(() =>
      expect(within(list).queryByText("プロダクト定例")).not.toBeInTheDocument(),
    );
    expect(await screen.findByDisplayValue("採用面談（バックエンド）")).toBeInTheDocument();
    expect(await backend.listNotes()).toHaveLength(2);
  });

  it("[NOTE-014] 削除を取り消したら何もしない", async () => {
    const user = userEvent.setup();
    const backend = renderApp(createMemoryBackend({ seed: true, confirm: () => false }));
    await screen.findByDisplayValue("プロダクト定例");
    await user.click(screen.getByRole("button", { name: "ノートを削除" }));
    expect(await backend.listNotes()).toHaveLength(3);
  });
});

describe("ノートの作成", () => {
  it("[NOTE-001] 新しいノートは空で一覧の先頭に加わり、検索欄を空に戻す", async () => {
    const user = userEvent.setup();
    renderApp(createMemoryBackend({ seed: true }));
    const list = await screen.findByRole("navigation", { name: "ノート一覧" });
    await within(list).findByText("プロダクト定例");
    const search = screen.getByLabelText("ノートを検索");
    await user.type(search, "採用");
    expect(within(list).getAllByRole("button")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: /新しいノート/ }));

    expect(search).toHaveValue("");
    const title = await screen.findByLabelText("タイトル");
    await waitFor(() => expect(title).toHaveFocus());
    expect(title).toHaveValue("");
    expect(screen.getByLabelText("自分のメモ")).toHaveValue("");
    const items = within(list).getAllByRole("button");
    expect(items).toHaveLength(4);
    expect(items[0]).toHaveTextContent("無題のノート");
    expect(items[0]).toHaveAttribute("aria-current", "page");
  });

  it("[NOTE-002] Ctrl+N と Cmd+N で新しいノートを作る", async () => {
    const backend = renderApp(createMemoryBackend({ seed: true }));
    await screen.findByDisplayValue("プロダクト定例");

    fireEvent.keyDown(window, { key: "n", ctrlKey: true });
    await waitFor(async () => expect(await backend.listNotes()).toHaveLength(4));
    await waitFor(() => expect(screen.getByLabelText("タイトル")).toHaveValue(""));
    expect(screen.getByLabelText("タイトル")).toHaveFocus();

    fireEvent.keyDown(window, { key: "N", metaKey: true });
    await waitFor(async () => expect(await backend.listNotes()).toHaveLength(5));

    // 修飾キーのない N では作らない
    fireEvent.keyDown(window, { key: "n" });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(await backend.listNotes()).toHaveLength(5);
  });

  it("[NOTE-003] ノートがないときは一覧にもそう表示し、案内のボタンから作れる", async () => {
    const user = userEvent.setup();
    renderApp();
    const list = await screen.findByRole("navigation", { name: "ノート一覧" });
    expect(within(list).getByText("まだノートがありません")).toBeInTheDocument();
    const guide = (await screen.findByText("最初の会議ノートを作りましょう")).closest(".empty-state") as HTMLElement;

    await user.click(within(guide).getByRole("button", { name: "新しいノート" }));

    expect(await screen.findByLabelText("タイトル")).toBeInTheDocument();
    expect(screen.queryByText("最初の会議ノートを作りましょう")).not.toBeInTheDocument();
    expect(within(list).queryByText("まだノートがありません")).not.toBeInTheDocument();
  });
});

describe("ノートの編集と自動保存", () => {
  async function seeded() {
    const backend = createMemoryBackend({ seed: true, confirm: () => true });
    const update = vi.spyOn(backend, "updateNote");
    renderApp(backend);
    const title = await screen.findByDisplayValue("プロダクト定例");
    const noteId = (await backend.listNotes())[0].id;
    return { backend, update, title, noteId };
  }

  it("[NOTE-009] 入力が止まって 0.5 秒たつと 1 回だけ保存し、保存の状態を表示する", async () => {
    const user = userEvent.setup();
    const { update, title, noteId } = await seeded();

    await user.type(title, "AB");
    expect(screen.getByRole("status")).toHaveTextContent("保存中…");
    expect(update).not.toHaveBeenCalled();

    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("保存しました"), { timeout: 2000 });
    expect(update).toHaveBeenCalledTimes(1);
    expect(update).toHaveBeenCalledWith(noteId, { title: "プロダクト定例AB" });
    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    expect(within(list).getByText("プロダクト定例AB")).toBeInTheDocument();
  });

  it("[NOTE-009] 保存できなかったときは、その旨と理由を表示する", async () => {
    const user = userEvent.setup();
    const { update, title } = await seeded();
    update.mockRejectedValue("データベースエラー: database or disk is full");

    await user.type(title, "A");

    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("保存できませんでした"), { timeout: 2000 });
    expect(screen.getByText("データベースエラー: database or disk is full")).toBeInTheDocument();
  });

  it("[NOTE-010] メモ欄からフォーカスが外れたら、0.5 秒を待たずに保存する", async () => {
    const user = userEvent.setup();
    const { update, noteId } = await seeded();
    const body = screen.getByLabelText("自分のメモ");

    await user.type(body, "{Enter}- 追記");
    expect(update).not.toHaveBeenCalled();
    await user.tab();

    expect(update).toHaveBeenCalledTimes(1);
    expect(update.mock.calls[0][0]).toBe(noteId);
    expect(update.mock.calls[0][1].userNotes).toMatch(/- 価格改定は来期に持ち越し\n- 追記$/);
  });

  it("[NOTE-010] 別のノートに切り替えるとき、未保存の入力を保存する", async () => {
    const user = userEvent.setup();
    const { backend, update, title, noteId } = await seeded();

    await user.type(title, "（改）");
    expect(update).not.toHaveBeenCalled();
    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    await user.click(within(list).getByText("採用面談（バックエンド）"));

    await screen.findByDisplayValue("採用面談（バックエンド）");
    expect(update).toHaveBeenCalledWith(noteId, { title: "プロダクト定例（改）" });
    expect((await backend.getNote(noteId)).title).toBe("プロダクト定例（改）");
    expect(await within(list).findByText("プロダクト定例（改）")).toBeInTheDocument();
  });

  it("[NOTE-010] 議事録の作成を始めるとき、未保存の入力を先に保存する", async () => {
    const user = userEvent.setup();
    const { backend, update, title, noteId } = await seeded();
    await backend.setApiKey("anthropic", "sk-ant-test");
    const enhance = vi.spyOn(backend, "enhanceNote");

    await user.type(title, "（改）");
    await user.click(screen.getByRole("button", { name: "議事録を作成" }));

    await waitFor(() => expect(enhance).toHaveBeenCalledWith(noteId));
    expect(update).toHaveBeenCalledWith(noteId, { title: "プロダクト定例（改）" });
    expect(update.mock.invocationCallOrder[0]).toBeLessThan(enhance.mock.invocationCallOrder[0]);
    await screen.findByRole("button", { name: "作り直す" }, { timeout: 3000 });
  });

  it("[NOTE-011] タイトル欄で Enter を押すとメモ欄へ移る（変換を確定する Enter では移らない）", async () => {
    const { title } = await seeded();
    title.focus();

    fireEvent.keyDown(title, { key: "Enter", isComposing: true });
    expect(title).toHaveFocus();

    fireEvent.keyDown(title, { key: "Enter" });
    expect(screen.getByLabelText("自分のメモ")).toHaveFocus();
    expect(title).toHaveValue("プロダクト定例");
  });

  it("[NOTE-013] 議事録のあるノートは議事録の表示で、ないノートは自分のメモの表示で開く", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend({ seed: true });
    const [first] = await backend.listNotes();
    await backend.updateNote(first.id, { enhancedNotes: "## 決定事項\n- 12/5 にリリース" });
    renderApp(backend);

    await screen.findByDisplayValue("プロダクト定例");
    expect(await screen.findByRole("tab", { name: "議事録" })).toHaveAttribute("aria-selected", "true");
    expect(within(screen.getByRole("region", { name: "議事録" })).getByText("12/5 にリリース")).toBeInTheDocument();
    expect(screen.queryByLabelText("自分のメモ")).not.toBeInTheDocument();

    const list = screen.getByRole("navigation", { name: "ノート一覧" });
    await user.click(within(list).getByText("採用面談（バックエンド）"));
    await screen.findByDisplayValue("採用面談（バックエンド）");
    expect(screen.getByLabelText("自分のメモ")).toHaveValue("- Rust 経験 3 年\n- 前職でストリーミング基盤\n- 入社可能時期: 2 月");
    expect(screen.queryByRole("tab", { name: "議事録" })).not.toBeInTheDocument();
  });
});

describe("失敗の知らせ", () => {
  it("[NOTE-020] 一覧を読み込めなかったら、理由を知らせる", async () => {
    const backend = createMemoryBackend();
    vi.spyOn(backend, "listNotes").mockRejectedValue("データベースエラー: unable to open database file");
    renderApp(backend);
    expect(await screen.findByRole("alert")).toHaveTextContent("データベースエラー: unable to open database file");
  });

  it("[ERR-001] 知らせは × で閉じられる", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend();
    vi.spyOn(backend, "listNotes").mockRejectedValue("データベースエラー: unable to open database file");
    renderApp(backend);
    const alert = await screen.findByRole("alert");
    // 文に「API キー」がなければ、設定へのボタンは付かない
    expect(within(alert).queryByRole("button", { name: "設定を開く" })).not.toBeInTheDocument();

    await user.click(within(alert).getByRole("button", { name: "お知らせを閉じる" }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("[ERR-011] ノートを開けなかったら、ノートの場所に理由を表示する", async () => {
    const backend = createMemoryBackend({ seed: true });
    vi.spyOn(backend, "getNote").mockRejectedValue("ノートが見つかりません: gone");
    renderApp(backend);

    expect(await screen.findByText("ノートが見つかりません: gone")).toBeInTheDocument();
    expect(screen.queryByLabelText("タイトル")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("[ERR-011] 理由の分からない失敗は「不明なエラーが発生しました」と表示する", async () => {
    const backend = createMemoryBackend({ seed: true });
    vi.spyOn(backend, "getNote").mockRejectedValue({ code: 42 });
    renderApp(backend);
    expect(await screen.findByText("不明なエラーが発生しました")).toBeInTheDocument();
  });
});

describe("検索", () => {
  async function searchable() {
    const backend = createMemoryBackend({ seed: true });
    const [product, hiring, untitled] = await backend.listNotes();
    // 議事録（プレビューは「候補者の印象」）と、文字起こし
    await backend.updateNote(hiring.id, { enhancedNotes: "## 概要\n- 候補者の印象\n\n## 決定事項\n- 二次面接に進める" });
    backend.emitSegment(untitled.id, "them", "来月の Offsite の場所を決めましょう");
    renderApp(backend);
    const list = await screen.findByRole("navigation", { name: "ノート一覧" });
    await within(list).findByText("プロダクト定例");
    const titles = () =>
      Array.from(list.querySelectorAll(".note-item-title > span:first-child")).map((element) => element.textContent);
    return { backend, list, titles, product, hiring, untitled, search: screen.getByLabelText("ノートを検索") };
  }

  it("[NOTE-008] メモの本文・議事録・文字起こしに含まれる文字でも見つかる（大文字と小文字は区別しない）", async () => {
    const user = userEvent.setup();
    const { titles, search } = await searchable();

    // メモの 3 行目（プレビューには出ていない）
    await user.type(search, "価格改定");
    await waitFor(() => expect(titles()).toEqual(["プロダクト定例"]));

    // 議事録の本文
    await user.clear(search);
    await user.type(search, "二次面接");
    await waitFor(() => expect(titles()).toEqual(["採用面談（バックエンド）"]));

    // 文字起こし
    await user.clear(search);
    await user.type(search, "offsite");
    await waitFor(() => expect(titles()).toEqual(["無題のノート"]));

    // タイトルとプレビュー（これまでどおり）
    await user.clear(search);
    await user.type(search, "無題");
    await waitFor(() => expect(titles()).toEqual(["無題のノート"]));

    await user.clear(search);
    await user.type(search, "どこにもない文字");
    expect(await screen.findByText("一致するノートがありません")).toBeInTheDocument();

    await user.clear(search);
    await waitFor(() => expect(titles()).toHaveLength(3));
  });

  it("[NOTE-008] 検索している間にノートの内容が変わったら、結果も変わる", async () => {
    const user = userEvent.setup();
    const { backend, titles, search, untitled } = await searchable();
    await user.type(search, "予算");
    expect(await screen.findByText("一致するノートがありません")).toBeInTheDocument();

    // 開いているノートのメモに書くと、見つかるようになる
    await user.type(screen.getByLabelText("自分のメモ"), "{Enter}- 予算は来期に");
    await waitFor(() => expect(titles()).toEqual(["プロダクト定例"]), { timeout: 3000 });
    expect(await backend.searchNotes("予算")).not.toContain(untitled.id);
  });

  it("[NOTE-008] 本文まで含めた検索ができないときも、タイトルとプレビューでは絞り込める", async () => {
    const user = userEvent.setup();
    const backend = createMemoryBackend({ seed: true });
    vi.spyOn(backend, "searchNotes").mockRejectedValue("コマンドを呼び出せません");
    renderApp(backend);
    const list = await screen.findByRole("navigation", { name: "ノート一覧" });
    await within(list).findByText("プロダクト定例");

    await user.type(screen.getByLabelText("ノートを検索"), "採用");

    expect(within(list).getByText("採用面談（バックエンド）")).toBeInTheDocument();
    expect(within(list).queryByText("プロダクト定例")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});
