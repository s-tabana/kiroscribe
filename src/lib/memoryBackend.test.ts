import { describe, expect, it } from "vitest";
import { createMemoryBackend } from "./memoryBackend";

// 画面のテストが相手にする模擬のバックエンドが、本体（Rust 側）と同じ答えを返すことを確かめる。
// 本体そのもののテストではないので、仕様の ID は付けない（本体の側は、コマンドの層のテストで確かめる）。
describe("模擬のバックエンド: 録音の状態と、本体と同じエラー", () => {
  async function recording() {
    const backend = createMemoryBackend({ seed: true });
    await backend.setApiKey("openai", "sk-test");
    const [first, second] = await backend.listNotes();
    await backend.startRecording(first.id);
    return { backend, first, second };
  }

  it("仕上げ中に開始すると、仕上げ中であることを伝えるエラーになる（同じノートでも）", async () => {
    const { backend, first, second } = await recording();
    const stopping = backend.stopRecording();
    expect((await backend.getRecordingStatus()).state).toBe("finishing");

    const message = "前の録音の文字起こしを仕上げています。少し待ってから開始してください";
    await expect(backend.startRecording(second.id)).rejects.toBe(message);
    await expect(backend.startRecording(first.id)).rejects.toBe(message);

    await stopping;
    expect((await backend.startRecording(second.id)).state).toBe("recording");
  });

  it("別のノートで録音中に開始するとエラーになり、同じノートなら今の状態を返す", async () => {
    const { backend, first, second } = await recording();
    await expect(backend.startRecording(second.id)).rejects.toBe(
      "別のノートで録音中です。先にそちらの録音を停止してください",
    );
    // 録音中かどうかを先に確かめる（存在しないノートでも、同じエラーになる）
    await expect(backend.startRecording("no-such-note")).rejects.toBe(
      "別のノートで録音中です。先にそちらの録音を停止してください",
    );
    const again = await backend.startRecording(first.id);
    expect(again).toEqual(await backend.getRecordingStatus());
    expect(again.state).toBe("recording");
  });

  it("一時停止中の一時停止と、録音中の再開は、エラーにせず状態を変えない", async () => {
    const { backend } = await recording();
    const recordingStatus = await backend.resumeRecording();
    expect(recordingStatus.state).toBe("recording");

    const paused = await backend.pauseRecording();
    expect(paused.state).toBe("paused");
    expect(await backend.pauseRecording()).toEqual(paused);
  });

  it("録音していないとき（仕上げ中を含む）の一時停止・再開は「録音していません」になり、停止はエラーにしない", async () => {
    const { backend } = await recording();
    const stopping = backend.stopRecording();
    await expect(backend.pauseRecording()).rejects.toBe("録音していません");
    await expect(backend.resumeRecording()).rejects.toBe("録音していません");
    await stopping;

    await expect(backend.pauseRecording()).rejects.toBe("録音していません");
    await expect(backend.resumeRecording()).rejects.toBe("録音していません");
    expect((await backend.stopRecording()).state).toBe("idle");
  });

  it("存在しないノートやキーがないときの開始は、本体と同じ文のエラーになる", async () => {
    const backend = createMemoryBackend({ seed: true });
    const [first] = await backend.listNotes();
    await expect(backend.startRecording("no-such-note")).rejects.toBe("ノートが見つかりません: no-such-note");
    await expect(backend.startRecording(first.id)).rejects.toBe(
      "API キーが設定されていません（OpenAI）。設定画面から入力してください",
    );
  });
});
