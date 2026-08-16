"use client";

import React, { memo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/cjs/styles/prism";
import { Check, Copy, ChevronDown, ChevronUp } from "lucide-react";
import { cn } from "@/lib/utils";

interface MarkdownContentProps {
  content: string;
}

/**
 * What react-markdown actually hands the `code` renderer: the `<code>` props
 * plus an `inline` flag telling a span of backticks from a fenced block.
 */
type CodeProps = React.ComponentPropsWithoutRef<"code"> & { inline?: boolean };

/**
 * Memoised, because of what a `snippet` memory costs.
 *
 * `react-markdown` re-parses its input on every render, and `SyntaxHighlighter`
 * re-tokenises every fenced block along with it. This is mounted once per result
 * card, and `EchoPanel` re-renders on every keystroke because the draft lives
 * there — so before this, typing re-rendered the previous results per character.
 *
 * Measured on this machine, eight cards' worth:
 *
 * | what the results hold        | per keystroke |
 * |------------------------------|---------------|
 * | prose memories               | ~2 ms         |
 * | `snippet` memories, fenced   | ~103 ms       |
 *
 * Two milliseconds is nothing and would not justify a `memo`. A hundred is six
 * dropped frames per character, and a result list of code snippets is an
 * ordinary thing to search for — `snippet` is one of the four kinds. So the
 * cheap case pays a comparison it does not need in order to keep the expensive
 * case from making the field unusable.
 *
 * The prop is one string, so that comparison is free and exact.
 */
export const MarkdownContent = memo(function MarkdownContent({
  content,
}: MarkdownContentProps) {
  return (
    <div className="prose prose-sm dark:prose-invert max-w-none break-words">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          // `inline` is passed by react-markdown but absent from the JSX
          // intrinsic props it is typed against, so it is declared here rather
          // than widening the whole signature to `any`.
          code({ className, children, ...props }: CodeProps) {
            const { inline, ...rest } = props;
            const match = /language-(\w+)/.exec(className || "");
            const language = match ? match[1] : "";
            const isInline = inline || !match;

            if (isInline) {
              return (
                <code
                  className={cn(
                    "rounded-md bg-muted px-[0.3rem] py-[0.2rem] font-mono text-[0.85em] text-foreground",
                    className
                  )}
                  {...rest}
                >
                  {children}
                </code>
              );
            }

            return (
              <CollapsibleCodeBlock language={language} code={String(children).replace(/\n$/, "")} />
            );
          },
          a({ href, children }) {
            return (
              <a href={href} target="_blank" rel="noreferrer" className="text-sky-500 hover:underline">
                {children}
              </a>
            );
          },
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
});

function CollapsibleCodeBlock({ language, code }: { language: string; code: string }) {
  const [copied, setCopied] = useState(false);
  const [expanded, setExpanded] = useState(false);
  
  // Calculate approximate lines
  const lines = code.split("\n").length;
  const isLong = lines > 15;

  const handleCopy = () => {
    navigator.clipboard.writeText(code);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="relative my-4 overflow-hidden rounded-xl border border-border/50 bg-[#282c34]">
      <div className="flex items-center justify-between px-4 py-2 text-xs text-muted-foreground border-b border-white/10 bg-white/[0.02]">
        <span className="font-mono lowercase">{language || "text"}</span>
        <button
          onClick={handleCopy}
          className="flex items-center gap-1.5 rounded-md px-2 py-1 transition-colors hover:bg-white/10 hover:text-white"
          aria-label="Copiar código"
        >
          {copied ? (
            <>
              <Check className="size-3.5 text-emerald-500" />
              <span className="text-emerald-500">Copiado</span>
            </>
          ) : (
            <>
              <Copy className="size-3.5" />
              <span>Copiar</span>
            </>
          )}
        </button>
      </div>
      
      <div className={cn("relative", !expanded && isLong && "max-h-[300px] overflow-hidden")}>
        <SyntaxHighlighter
          style={oneDark}
          language={language}
          PreTag="div"
          customStyle={{
            margin: 0,
            background: "transparent",
            padding: "1rem",
            fontSize: "13px",
            lineHeight: "1.5",
          }}
        >
          {code}
        </SyntaxHighlighter>

        {!expanded && isLong && (
          <div className="absolute bottom-0 left-0 right-0 h-24 bg-gradient-to-t from-[#282c34] to-transparent pointer-events-none" />
        )}
      </div>
      
      {isLong && (
        <button
          onClick={() => setExpanded(!expanded)}
          className="flex w-full items-center justify-center gap-2 border-t border-white/5 py-2 text-[11px] font-medium text-muted-foreground transition-colors hover:bg-white/[0.02] hover:text-white"
        >
          {expanded ? (
            <>
              Recolher <ChevronUp className="size-3.5" />
            </>
          ) : (
            <>
              Expandir código ({lines} linhas) <ChevronDown className="size-3.5" />
            </>
          )}
        </button>
      )}
    </div>
  );
}
