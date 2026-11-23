import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { errorMessage, useBackend } from "../lib/backend";
import { currentChatRun, startChatRun, takeChatDraft, type ChatRun } from "../lib/chatRuns";
import type { ChatMessage } from "../lib/types";
import { Markdown } from "./Markdown";
import { SendIcon, TrashIcon } from "./icons";

const SUGGESTIONS = [
  "この会議で決まったことは？",
  "自分の ToDo をまとめて",
  "相手の懸念点は？",
  "フォローアップのメール文案を作って",
];

interface ChatPanelProps {
  noteId: string;
  onError(message: string): void;
}

/** ノートについて AI に質問するパネル */
export function ChatPanel({ noteId, onError }: ChatPanelProps) {
  const backend = useBackend();
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  // 作成中の回答は、パネルの外（chatRuns）で持つ。パネルを閉じて開き直しても、続きから表示できる
  const [run, setRun] = useState<ChatRun | null>(() => currentChatRun(backend, noteId));
  const [answer, setAnswer] = useState(() => run?.answer ?? "");
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const pending = run?.question ?? null;

  useEffect(() => {
    let active = true;
    void backend.listChatMessages(noteId).then((list) => active && setMessages(list));
    // パネルを閉じているあいだに回答をもらえなかった質問があれば、入力欄に戻す
    const draft = takeChatDraft(backend, noteId);
    if (draft) setInput((current) => current || draft);
    return () => {
      active = false;
    };
  }, [backend, noteId]);

  // 作成中の回答を追いかける。終わったら（失敗・中止でも）、履歴を読み直して、送信できる状態に戻す
  useEffect(() => {
    if (!run) return;
    let active = true;
    setAnswer(run.answer);
    const unsubscribe = run.subscribe(() => active && setAnswer(run.answer));
    const finish = async () => {
      const list = await backend.listChatMessages(noteId).catch(() => null);
      if (!active) return;
      if (list) setMessages(list);
      setRun(null);
      setAnswer("");
      // 送れなかった質問は入力欄に戻す
      const draft = takeChatDraft(backend, noteId);
      if (draft) setInput((current) => current || draft);
      inputRef.current?.focus();
    };
    run.done.then(finish, finish);
    return () => {
      active = false;
      unsubscribe();
    };
  }, [backend, noteId, run]);

  useLayoutEffect(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages.length, answer, pending]);

  const send = (text: string) => {
    const question = text.trim();
    if (!question || pending !== null) return;
    setAnswer("");
    setInput("");
    const started = startChatRun(backend, noteId, question);
    // 失敗は、パネルを閉じていても知らせる（中止は知らせない）
    started.done.catch((err: unknown) => {
      const message = errorMessage(err);
      if (message !== "中止しました") onError(message);
    });
    setRun(started);
  };

  const clear = async () => {
    if (!(await backend.confirm("このノートの AI チャットの履歴を消去しますか？", "チャットの消去"))) return;
    await backend.clearChatMessages(noteId);
    setMessages([]);
  };

  // 作成中の質問は、送る前に履歴へ保存されている。開き直したときに読み込んだ履歴の末尾がその質問なら、
  // 作成中の表示（質問と、届いた分の回答）と重ならないように除く
  const last = messages[messages.length - 1];
  const history = pending !== null && last?.role === "user" && last.content === pending ? messages.slice(0, -1) : messages;
  const empty = history.length === 0 && pending === null;

  return (
    <div className="chat">
      <div className="chat-list" ref={listRef}>
        {empty ? (
          <div className="chat-empty">
            <p>このノートのメモ・文字起こし・議事録をもとに、AI が答えます。</p>
            <div className="suggestions">
              {SUGGESTIONS.map((s) => (
                <button key={s} type="button" className="chip" onClick={() => send(s)}>
                  {s}
                </button>
              ))}
            </div>
          </div>
        ) : (
          <>
            {history.map((m) => (
              <div key={m.id} className={`chat-message from-${m.role}`}>
                {m.role === "assistant" ? <Markdown text={m.content} /> : <p>{m.content}</p>}
              </div>
            ))}
            {pending !== null && (
              <>
                <div className="chat-message from-user">
                  <p>{pending}</p>
                </div>
                <div className="chat-message from-assistant" aria-live="polite">
                  {answer ? <Markdown text={answer} /> : <p className="thinking">考えています…</p>}
                </div>
              </>
            )}
          </>
        )}
      </div>

      <form
        className="chat-input"
        onSubmit={(e) => {
          e.preventDefault();
          send(input);
        }}
      >
        <textarea
          ref={inputRef}
          value={input}
          rows={2}
          placeholder="この会議について質問（Enter で送信、Shift+Enter で改行）"
          aria-label="AI への質問"
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              send(input);
            }
          }}
        />
        <div className="chat-actions">
          {messages.length > 0 && pending === null && (
            <button type="button" className="icon-button" onClick={() => void clear()} aria-label="チャットの履歴を消去" title="履歴を消去">
              <TrashIcon size={15} />
            </button>
          )}
          {pending !== null ? (
            <button type="button" className="button" onClick={() => void backend.cancelAi(noteId, "chat")}>
              中止
            </button>
          ) : (
            <button type="submit" className="button button-primary" disabled={!input.trim()} aria-label="送信">
              <SendIcon size={15} />
              送信
            </button>
          )}
        </div>
      </form>
    </div>
  );
}
