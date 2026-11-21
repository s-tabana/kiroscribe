import { useMemo } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { classifyLines, type LineSource } from "../lib/attribution";

interface MarkdownProps {
  text: string;
  /** 指定すると、各行を「自分のメモ由来」「AI が補ったもの」で色分けする */
  userNotes?: string;
  /**
   * 指定すると、https:// のリンクを押せるようにし、押されたらこれを呼ぶ（ヘルプ用。外部ブラウザで開く）。
   * 議事録では指定しない（リンクは無効のまま）
   */
  onOpenLink?(url: string): void;
}

type WithNode = { node?: { position?: { start: { line: number } } } };

const isExternalUrl = (href: string | undefined): href is string => !!href && /^https:\/\//i.test(href);

/**
 * Markdown を表示する。既定ではリンクを無効化する（議事録に URL を埋め込ませない）。画像は常に表示しない。
 * リンクを有効にする場合も href は付けず、アプリの画面そのものが遷移しないようにする。
 */
export function Markdown({ text, userNotes, onOpenLink }: MarkdownProps) {
  const sources = useMemo(
    () => (userNotes === undefined ? null : classifyLines(text, userNotes)),
    [text, userNotes],
  );

  const components = useMemo<Components>(() => {
    const sourceClass = (props: WithNode): string | undefined => {
      const line = props.node?.position?.start.line;
      const source: LineSource | undefined = line ? sources?.get(line) : undefined;
      return source && source !== "neutral" ? `src-${source}` : undefined;
    };
    return {
      li: ({ node, children, className, ...rest }) => (
        <li {...rest} className={[className, sourceClass({ node })].filter(Boolean).join(" ") || undefined}>
          {children}
        </li>
      ),
      p: ({ node, children, ...rest }) => (
        <p {...rest} className={sourceClass({ node })}>
          {children}
        </p>
      ),
      a: ({ children, href }) => {
        if (!onOpenLink || !isExternalUrl(href)) return <span className="md-link">{children}</span>;
        return (
          <a
            className="md-link is-external"
            role="link"
            tabIndex={0}
            title={href}
            onClick={() => onOpenLink(href)}
            onKeyDown={(e) => e.key === "Enter" && onOpenLink(href)}
          >
            {children}
          </a>
        );
      },
      img: () => null,
    };
  }, [sources, onOpenLink]);

  return (
    <div className="markdown">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
        {text}
      </ReactMarkdown>
    </div>
  );
}
