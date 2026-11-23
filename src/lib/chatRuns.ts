import type { Backend } from "./backend";
import type { ChatMessage } from "./types";

/**
 * 進行中の AI チャットの回答。
 *
 * 回答の作成は、AI チャットのパネルを閉じても、別のノートへ移っても、裏で続く。パネルは開くたびに作り直されるので、
 * 進み具合をパネルの中だけで持つと、開き直したときに「作成中かどうか」「どこまで届いたか」が分からなくなり、
 * 終わっても回答が出ない。そこで、作成中のものをパネルの外（ここ）で持ち、開き直したパネルが続きから表示できるようにする
 * （議事録の作成の `enhanceRuns` と同じ考え方）。
 */
export interface ChatRun {
  /** 送った質問 */
  readonly question: string;
  /** これまでに届いた回答 */
  readonly answer: string;
  /** 作成の結果（保存された回答）。失敗・中止したときは reject する */
  readonly done: Promise<ChatMessage>;
  /** 回答が届くたびに呼ばれる。戻り値で解除する */
  subscribe(listener: () => void): () => void;
}

interface Store {
  runs: Map<string, ChatRun>;
  /** 回答をもらえなかった質問（次にパネルが入力欄へ戻す） */
  drafts: Map<string, string>;
}

const stores = new WeakMap<Backend, Store>();

function storeOf(backend: Backend): Store {
  let store = stores.get(backend);
  if (!store) {
    store = { runs: new Map(), drafts: new Map() };
    stores.set(backend, store);
  }
  return store;
}

/** そのノートの回答を作成中なら、その作成を返す */
export function currentChatRun(backend: Backend, noteId: string): ChatRun | null {
  return storeOf(backend).runs.get(noteId) ?? null;
}

/** 質問を送って、回答の作成を始める。すでに作成中なら、進行中のものを返す（二重には送らない） */
export function startChatRun(backend: Backend, noteId: string, question: string): ChatRun {
  const { runs, drafts } = storeOf(backend);
  const running = runs.get(noteId);
  if (running) return running;

  drafts.delete(noteId);
  let answer = "";
  const listeners = new Set<() => void>();
  // 届いた回答は、送る前から受け取り始める
  const unsubscribe = backend.onChatDelta((e) => {
    if (e.noteId !== noteId) return;
    answer += e.delta;
    listeners.forEach((listener) => listener());
  });
  const done = backend
    .sendChatMessage(noteId, question)
    .catch((err: unknown) => {
      // 回答をもらえなかった質問は履歴に残らないので、入力欄に戻せるように取っておく
      drafts.set(noteId, question);
      throw err;
    })
    .finally(() => {
      unsubscribe();
      if (runs.get(noteId) === run) runs.delete(noteId);
    });
  // 結果を待つパネルがなくなっていても、未処理の reject として扱われないようにする
  done.catch(() => undefined);
  const run: ChatRun = {
    question,
    get answer() {
      return answer;
    },
    done,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
  runs.set(noteId, run);
  return run;
}

/** 回答をもらえなかった質問があれば、取り出す（入力欄に戻すのは 1 回だけ） */
export function takeChatDraft(backend: Backend, noteId: string): string | null {
  const { drafts } = storeOf(backend);
  const draft = drafts.get(noteId) ?? null;
  drafts.delete(noteId);
  return draft;
}
