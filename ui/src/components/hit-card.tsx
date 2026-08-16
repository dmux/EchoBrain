"use client";

import { KindBadge } from "@/components/kind-badge";
import { MarkdownContent } from "@/components/markdown-content";
import { SimilarityMeter } from "@/components/similarity-meter";
import type { Hit, MemoryGateway } from "@/lib/memory";
import { Volume2, Square, Terminal, GitBranch, User } from "lucide-react";
import { memo, useEffect, useState } from "react";

/**
 * Memoised for the same reason `MarkdownContent` is, one level up.
 *
 * A result list is up to eight of these, and they hang off a panel that
 * re-renders on every keystroke. None of their inputs change while the user
 * types — `hit` comes from the settled results, and `gateway` is a `useMemo` on
 * the panel — so the whole list can sit still instead of being rebuilt per
 * character.
 */
export const HitCard = memo(function HitCard({ hit, index, gateway }: { hit: Hit; index: number; gateway: MemoryGateway }) {
  const [canSpeak, setCanSpeak] = useState(false);
  const [playing, setPlaying] = useState(false);

  useEffect(() => {
    gateway.speechAvailable().then(setCanSpeak).catch(() => setCanSpeak(false));
  }, [gateway]);

  const handleTogglePlay = async () => {
    if (playing) {
      await gateway.stopSpeaking();
      setPlaying(false);
    } else {
      setPlaying(true);
      try {
        await gateway.speak(hit.memory.text);
      } finally {
        setPlaying(false);
      }
    }
  };

  return (
    <article
      className="group animate-in fade-in slide-in-from-bottom-2 rounded-xl border border-border/60 bg-card/60 p-4 backdrop-blur-sm transition-colors duration-200 hover:border-border hover:bg-card/90 fill-mode-backwards"
      // Staggered entry: results land in reading order rather than all at once.
      style={{ animationDelay: `${Math.min(index, 8) * 45}ms` }}
    >
      <div className="mb-2 flex items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <KindBadge kind={hit.memory.kind} />
          {canSpeak && hit.memory.kind !== "snippet" && (
            <button
              onClick={() => void handleTogglePlay()}
              className="rounded-md p-1 text-muted-foreground hover:bg-muted/50 hover:text-foreground focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-1"
              aria-label={playing ? "Parar leitura" : "Ouvir"}
            >
              {playing ? (
                <Square className="size-3.5 fill-current" />
              ) : (
                <Volume2 className="size-3.5" />
              )}
            </button>
          )}
        </div>
        <SimilarityMeter value={hit.similarity} />
      </div>
      <MarkdownContent content={hit.memory.text} />
      <footer className="mt-2.5 flex items-center gap-2 text-[11px] text-muted-foreground/70">
        <time dateTime={new Date(hit.memory.createdAt * 1000).toISOString()}>
          {formatWhen(hit.memory.createdAt)}
        </time>
        {hit.memory.source ? (
          <>
            <span aria-hidden>·</span>
            <span className="flex items-center gap-1 rounded-md border border-border/50 bg-muted/30 px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground cursor-pointer">
              <Terminal className="size-3" />
              {hit.memory.source}
            </span>
          </>
        ) : null}
        {hit.memory.githubRepo ? (
          <>
            <span aria-hidden>·</span>
            <span className="flex items-center gap-1 rounded-md border border-border/50 bg-muted/30 px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground cursor-pointer" title="Repositório GitHub">
              <GitBranch className="size-3" />
              {hit.memory.githubRepo}
            </span>
          </>
        ) : null}
        {hit.memory.githubLogin ? (
          <>
            <span aria-hidden>·</span>
            <span className="flex items-center gap-1 rounded-md border border-border/50 bg-muted/30 px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground cursor-pointer" title="Usuário GitHub">
              <User className="size-3" />
              {hit.memory.githubLogin}
            </span>
          </>
        ) : null}
      </footer>
    </article>
  );
});

/** Relative for the recent past, absolute once "3 semanas atrás" stops helping. */
function formatWhen(unixSeconds: number): string {
  const seconds = Math.floor(Date.now() / 1000) - unixSeconds;
  const days = Math.floor(seconds / 86_400);

  if (seconds < 60) return "agora";
  if (seconds < 3_600) return `${Math.floor(seconds / 60)} min atrás`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3_600)} h atrás`;
  if (days === 1) return "ontem";
  if (days < 30) return `${days} dias atrás`;

  return new Date(unixSeconds * 1000).toLocaleDateString("pt-BR", {
    day: "2-digit",
    month: "short",
    year: "numeric",
  });
}
