import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SettingsDialog } from "./SettingsDialog";
import { BackendContext } from "../lib/backend";
import { createMemoryBackend } from "../lib/memoryBackend";
import { COMPANION_PRICES, hourlyCostText, hourlyCostUsd, indexStatusText } from "../lib/companion";
import type { Settings } from "../lib/types";

const FOLDER = "C:\\Users\\tabana\\Documents\\共有資料";

function setup(settings: Partial<Settings> = {}, pickFolder: () => string | null = () => FOLDER) {
  const backend = createMemoryBackend({ settings, pickFolder });
  const saveSettings = vi.spyOn(backend, "saveSettings");
  render(
    <BackendContext.Provider value={backend}>
      <SettingsDialog onClose={() => undefined} />
    </BackendContext.Provider>,
  );
  return { backend, saveSettings };
}

const section = async () => (await screen.findByRole("heading", { name: "会議の相棒" })).closest("section") as HTMLElement;
const companionCheck = () => screen.getByLabelText(/録音中に、聞かれたことに答える資料を探す（会議の相棒）/);

describe("会議の相棒の設定", () => {
  it("[CMP-036] 相棒は初期値でオン。送り先・送るもの・費用をいつも表示し、切り替えは「設定を保存」で保存する", async () => {
    const user = userEvent.setup();
    const { backend, saveSettings } = setup();
    const area = await section();
    expect(companionCheck()).toBeChecked();
    const list = within(area).getByLabelText("会議の相棒が PC の外に送るもの");
    expect(list).toHaveTextContent("Anthropic（Claude）");
    expect(list).toHaveTextContent("資料を求めていそうな発言");
    expect(list).toHaveTextContent("題名・場所");
    expect(list).toHaveTextContent("「中身を送らない」にしたフォルダは、抜粋を送りません");
    expect(list).toHaveTextContent("お使いの Anthropic の API キーに請求されます。1 時間の会議で最大 約 0.03 ドル");

    // チェックを外しただけでは保存しない（確認のダイアログも出さない）
    const confirm = vi.spyOn(backend, "confirm");
    await user.click(companionCheck());
    expect(companionCheck()).not.toBeChecked();
    expect(confirm).not.toHaveBeenCalled();
    expect((await backend.getSettings()).companion).toBe(true);
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    expect(saveSettings).toHaveBeenCalledOnce();
    expect((await backend.getSettings()).companion).toBe(false);
  });

  it("[CMP-036] 費用の目安は、相棒のモデルの単価で、判定 60 回・選択 15 回から計算する", () => {
    // 入力 60×1,500 + 15×4,500 = 157,500 トークン、出力 60×200 + 15×400 = 18,000 トークン
    expect(hourlyCostUsd("claude-haiku-5-5")).toBeCloseTo((157_500 * 0.1 + 18_000 * 0.5) / 1e6, 10);
    expect(hourlyCostUsd("claude-opus-5-5")).toBeCloseTo((157_500 * 4 + 18_000 * 20) / 1e6, 10);
    expect(hourlyCostText("claude-haiku-5-5")).toBe("1 時間の会議で最大 約 0.03 ドル");
    expect(hourlyCostText("claude-opus-5-5")).toBe("1 時間の会議で最大 約 0.99 ドル");
    expect(hourlyCostText("claude-sonnet-5-5")).toBe("1 時間の会議で最大 約 0.50 ドル");
    expect(hourlyCostText("claude-fable-5-2")).toBe("1 時間の会議で最大 約 1.98 ドル");
    expect(hourlyCostText("claude-future-9")).toBe("このモデルの料金は、Anthropic の料金表で確かめてください");
    expect(Object.keys(COMPANION_PRICES).sort()).toEqual(
      ["claude-fable-5-2", "claude-haiku-5-5", "claude-opus-5-5", "claude-sonnet-5-5"],
    );
  });

  it("[CMP-037] 相棒のモデルは Claude のモデルと同じ選択肢から選べ、初期値は claude-haiku-5-5。一覧にない値は残す", async () => {
    const user = userEvent.setup();
    setup();
    const area = await section();
    const select = within(area).getByLabelText("相棒のモデル") as HTMLSelectElement;
    expect(select.value).toBe("claude-haiku-5-5");
    expect(Array.from(select.options).map((o) => o.value)).toEqual([
      "claude-opus-5-5",
      "claude-sonnet-5-5",
      "claude-haiku-5-5",
      "claude-fable-5-2",
    ]);
    await user.selectOptions(select, "claude-opus-5-5");
    expect(within(area).getByLabelText("会議の相棒が PC の外に送るもの")).toHaveTextContent("約 0.99 ドル");
  });

  it("[CMP-037] 一覧にない相棒のモデルが保存されているときは、末尾に出して選んだままにする", async () => {
    const { backend } = setup({ companionModel: "claude-future-9" });
    const area = await section();
    const select = within(area).getByLabelText("相棒のモデル") as HTMLSelectElement;
    expect(select.value).toBe("claude-future-9");
    expect(Array.from(select.options).at(-1)?.value).toBe("claude-future-9");
    await userEvent.setup().click(screen.getByRole("button", { name: "設定を保存" }));
    await waitFor(async () => expect((await backend.getSettings()).companionModel).toBe("claude-future-9"));
  });

  it("[IDX-001][IDX-034] フォルダを足すと、その場で保存し、パス・件数・「中身を送らない」・「外す」を表示する", async () => {
    const user = userEvent.setup();
    const { backend, saveSettings } = setup();
    const area = await section();
    expect(within(area).getByText(/足したフォルダの中の資料の題名・場所・抜粋は、相棒が資料を選ぶときに Anthropic へ送られます/)).toBeInTheDocument();
    expect(within(area).queryByRole("list")).not.toBeInTheDocument();

    await user.click(within(area).getByRole("button", { name: "フォルダを追加…" }));
    const row = (await within(area).findByText(FOLDER)).closest("li") as HTMLElement;
    expect(within(row).getByText("12 件")).toBeInTheDocument();
    const noContent = within(row).getByLabelText("中身を送らない");
    expect(noContent).not.toBeChecked();
    expect(saveSettings).not.toHaveBeenCalled();
    expect((await backend.getSearchIndexStatus()).folders.map((f) => f.path)).toEqual([FOLDER]);

    await user.click(noContent);
    await waitFor(() => expect(within(row).getByLabelText("中身を送らない")).toBeChecked());
    expect((await backend.getSearchIndexStatus()).folders[0].noContent).toBe(true);
    expect(saveSettings).not.toHaveBeenCalled();

    // 同じフォルダは 2 回足さない
    await user.click(within(area).getByRole("button", { name: "フォルダを追加…" }));
    await waitFor(async () => expect((await backend.getSearchIndexStatus()).folders).toHaveLength(1));

    await user.click(within(row).getByRole("button", { name: `${FOLDER} を外す` }));
    await waitFor(() => expect(within(area).queryByText(FOLDER)).not.toBeInTheDocument());
    expect((await backend.getSearchIndexStatus()).folders).toEqual([]);
    expect(saveSettings).not.toHaveBeenCalled();
  });

  it("[IDX-001] フォルダを選ぶダイアログを取り消したら、何も足さない", async () => {
    const user = userEvent.setup();
    const { backend } = setup({}, () => null);
    const area = await section();
    await user.click(within(area).getByRole("button", { name: "フォルダを追加…" }));
    await waitFor(async () => expect((await backend.getSearchIndexStatus()).folders).toEqual([]));
    expect(within(area).queryByRole("list")).not.toBeInTheDocument();
  });

  it("[IDX-008][IDX-003][IDX-010] 索引の状態と、一部だけ入れたフォルダ・見つからないフォルダを表示する", async () => {
    const { backend } = setup();
    const area = await section();
    const folder = { noContent: false, missing: false, truncated: false };
    backend.setSearchIndexStatus({
      state: "building",
      done: 1234,
      total: 5000,
      folders: [
        { ...folder, path: "D:\\多い", files: 20000, truncated: true },
        { ...folder, path: "E:\\外した", files: 3, missing: true },
      ],
    });
    expect(await within(area).findByText("索引を作っています（1,234 / 5,000 件）")).toBeInTheDocument();
    const many = within(area).getByText("D:\\多い").closest("li") as HTMLElement;
    expect(many).toHaveTextContent("20,000 件・一部だけ索引に入れています（20,000 件まで）");
    const missing = within(area).getByText("E:\\外した").closest("li") as HTMLElement;
    expect(missing).toHaveTextContent("見つかりません");
    expect(within(area).getByRole("button", { name: "索引を作り直す" })).toBeDisabled();

    const updatedAt = new Date(2026, 10, 27, 11, 20).getTime();
    backend.setSearchIndexStatus({ state: "ready", done: 0, total: 0, files: 5000, updatedAt });
    expect(await within(area).findByText("索引に入っています: 5,000 件（11:20 に更新）")).toBeInTheDocument();
  });

  it("[IDX-008] 索引の状態の文", () => {
    const base = { done: 0, total: 0, files: 0, updatedAt: null, folders: [] };
    expect(indexStatusText({ ...base, state: "building", done: 1234, total: 5000 })).toBe("索引を作っています（1,234 / 5,000 件）");
    expect(indexStatusText({ ...base, state: "ready", files: 5000, updatedAt: new Date(2026, 0, 1, 9, 5).getTime() })).toBe(
      "索引に入っています: 5,000 件（9:05 に更新）",
    );
    expect(indexStatusText({ ...base, state: "off" })).toBe("会議の相棒がオフのため、索引は作っていません");
  });

  it("[IDX-011][IDX-028] 「索引を作り直す」は相棒がオンのときだけ押せる。オフにして保存すると、索引を空にする", async () => {
    const user = userEvent.setup();
    const { backend } = setup();
    const area = await section();
    await user.click(within(area).getByRole("button", { name: "フォルダを追加…" }));
    await within(area).findByText("12 件");
    const rebuild = vi.spyOn(backend, "rebuildSearchIndex");
    await user.click(within(area).getByRole("button", { name: "索引を作り直す" }));
    expect(rebuild).toHaveBeenCalledOnce();

    await user.click(companionCheck());
    await user.click(screen.getByRole("button", { name: "設定を保存" }));
    expect(await within(area).findByText("会議の相棒がオフのため、索引は作っていません")).toBeInTheDocument();
    expect(within(area).getByText("0 件")).toBeInTheDocument();
    expect(within(area).getByText(FOLDER)).toBeInTheDocument();
    expect(within(area).getByRole("button", { name: "索引を作り直す" })).toBeDisabled();
    await expect(backend.rebuildSearchIndex()).rejects.toBe("会議の相棒がオフのときは、索引を作り直せません");
  });
});
