import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { errorMessage, useBackend } from "../lib/backend";
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
  const [pending, setPending] = useState<string | null>(null);
  const [answer, setAnswer] = useState("");
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    let active = true;
    void backend.listChatMessages(noteId).then((list) => active && setMessages(list));
    const unsubscribe = backend.onChatDelta((e) => {
      if (e.noteId === noteId) setAnswer((prev) => prev + e.delta);
    });
    return () => {
      active = false;
      unsubscribe();
    };
  }, [backend, noteId]);

  useLayoutEffect(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages.length, answer, pending]);

  const send = async (text: string) => {
    const question = text.trim();
    if (!question || pending !== null) return;
    setPending(question);
    setAnswer("");
    setInput("");
    try {
      await backend.sendChatMessage(noteId, question);
    } catch (err) {
      const message = errorMessage(err);
      if (message !== "中止しました") onError(message);
      // 送れなかった質問は入力欄に戻す
      setInput((current) => current || question);
    } finally {
      setMessages(await backend.listChatMessages(noteId));
      setPending(null);
      setAnswer("");
      inputRef.current?.focus();
    }
  };

  const clear = async () => {
    if (!(await backend.confirm("このノートの AI チャットの履歴を消去しますか？", "チャットの消去"))) return;
    await backend.clearChatMessages(noteId);
    setMessages([]);
  };

  const empty = messages.length === 0 && pending === null;

  return (
    <div className="chat">
      <div className="chat-list" ref={listRef}>
        {empty ? (
          <div className="chat-empty">
            <p>このノートのメモ・文字起こし・議事録をもとに、AI が答えます。</p>
            <div className="suggestions">
              {SUGGESTIONS.map((s) => (
                <button key={s} type="button" className="chip" onClick={() => void send(s)}>
                  {s}
                </button>
              ))}
            </div>
          </div>
        ) : (
          <>
            {messages.map((m) => (
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
          void send(input);
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
              void send(input);
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
