import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
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
  it("ノートがないときは作成を促す", async () => {
    renderApp();
    expect(await screen.findByText("最初の会議ノートを作りましょう")).toBeInTheDocument();
  });

  it("新しいノートを作ってタイトルとメモを書くと、一覧に反映され保存される", async () => {
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

  it("検索で一覧を絞り込める", async () => {
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

  it("確認してからノートを削除し、次のノートを選ぶ", async () => {
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

  it("削除を取り消したら何もしない", async () => {
    const user = userEvent.setup();
    const backend = renderApp(createMemoryBackend({ seed: true, confirm: () => false }));
    await screen.findByDisplayValue("プロダクト定例");
    await user.click(screen.getByRole("button", { name: "ノートを削除" }));
    expect(await backend.listNotes()).toHaveLength(3);
  });
});
