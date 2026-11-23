import type { Backend } from "./backend";
import type { Note } from "./types";

/**
 * 進行中の議事録の作成。
 *
 * 作成は、ノートの画面（NoteView）を離れても裏で続く。画面はノートを切り替えるたびに作り直されるので、
 * 進み具合を画面の中だけで持つと、戻ってきたときに「作成中かどうか」「どこまで届いたか」が分からなくなる。
 * そこで、作成中のものを画面の外（ここ）で持ち、戻ってきた画面が続きから表示できるようにする。
 */
export interface EnhanceRun {
  /** これまでに届いた文章（先頭のタイトル行を含む） */
  readonly text: string;
  /** 作成の結果（保存されたノート）。失敗・中止したときは reject する */
  readonly done: Promise<Note>;
}

const runsByBackend = new WeakMap<Backend, Map<string, EnhanceRun>>();

function runsOf(backend: Backend): Map<string, EnhanceRun> {
  let runs = runsByBackend.get(backend);
  if (!runs) {
    runs = new Map();
    runsByBackend.set(backend, runs);
  }
  return runs;
}

/** そのノートの議事録を作成中なら、その作成を返す */
export function currentEnhanceRun(backend: Backend, noteId: string): EnhanceRun | null {
  return runsOf(backend).get(noteId) ?? null;
}

/** 議事録の作成を始める。すでに作成中なら、進行中のものを返す（二重には始めない） */
export function startEnhanceRun(backend: Backend, noteId: string): EnhanceRun {
  const runs = runsOf(backend);
  const running = runs.get(noteId);
  if (running) return running;

  // 届いた文章は、作成を呼び出す前から受け取り始める
  let text = "";
  const unsubscribe = backend.onEnhanceDelta((e) => {
    if (e.noteId === noteId) text += e.delta;
  });
  const done = backend.enhanceNote(noteId).finally(() => {
    unsubscribe();
    if (runs.get(noteId) === run) runs.delete(noteId);
  });
  // 結果を待つ画面がなくなっていても、未処理の reject として扱われないようにする
  done.catch(() => undefined);
  const run: EnhanceRun = {
    get text() {
      return text;
    },
    done,
  };
  runs.set(noteId, run);
  return run;
}
