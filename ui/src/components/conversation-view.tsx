"use client";

import { useEffect, useState } from "react";
import { Loader2, Mic, MicOff, Volume2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Waveform } from "@/components/waveform";
import { cn } from "@/lib/utils";
import {
  type ConversationPhase,
  type ConversationState,
  PHASE_LABELS,
  MemoryGateway,
} from "@/lib/memory";

/**
 * The conversation, while it is happening.
 *
 * # Why the microphone indicator is never softened
 *
 * The dot is lit whenever `microphoneOpen` is true, and that includes while the
 * app is talking — barge-in only works because the microphone never closed.
 * Showing it shut during the answer would look calmer and would be a lie about
 * a running microphone, which is the one thing an always-listening feature
 * cannot afford to be casual about.
 *
 * # Why the transcript is always shown
 *
 * Not a debug affordance. Without it the app is unusable for anyone who cannot
 * hear the answer, and speech becomes the only channel.
 */
export function ConversationView({
  state,
  gateway,
  onStop,
}: {
  state: ConversationState;
  gateway: MemoryGateway;
  onStop: () => void;
}) {
  const phase = state.phase;
  const [level, setLevel] = useState(0);

  useEffect(() => {
    if (!state.microphoneOpen) return;
    const timer = setInterval(async () => {
      try {
        setLevel(await gateway.audioLevel());
      } catch {
        // Ignore poll failures
      }
    }, 33); // ~30 fps
    return () => clearInterval(timer);
  }, [state.microphoneOpen, gateway]);

  const showWaveform = phase === "hearing" || phase === "listening" || phase === "follow_up";

  return (
    <section
      className="relative overflow-hidden animate-in fade-in slide-in-from-bottom-2 space-y-3 rounded-xl border border-sky-500/25 bg-gradient-to-br from-sky-500/[0.06] to-violet-500/[0.06] p-4 backdrop-blur-sm shadow-sm"
      aria-live="polite"
    >
      {showWaveform && (
        <div 
          className="absolute -inset-[2px] z-0 rounded-xl bg-gradient-to-r from-emerald-500 via-sky-500 to-emerald-500 opacity-20 blur-xl duration-500"
          style={{
            backgroundSize: "200% 100%",
            animation: "gradient-shift 3s ease-in-out infinite"
          }}
        />
      )}
      <div className="relative z-10 flex items-center gap-3">
        {showWaveform ? (
          <div className="flex h-12 w-12 items-center justify-center shrink-0">
            <Waveform level={level * 4} active={true} bars={5} className="h-8 gap-0.5" />
          </div>
        ) : (
          <Orb phase={phase} />
        )}

        <div className="min-w-0 flex-1">
          <p className="text-[13px] font-medium">{PHASE_LABELS[phase]}</p>
          <p className="text-[11.5px] text-muted-foreground">
            {HINTS[phase]}
          </p>
        </div>

        <div className="flex items-center gap-2">
          {state.microphoneOpen ? (
            <span
              className="flex items-center gap-1 rounded-full border border-red-500/25 bg-red-500/10 px-2 py-0.5 text-[10.5px] text-red-600 dark:text-red-300"
              title="O microfone está aberto"
            >
              <span className="size-1.5 animate-pulse rounded-full bg-red-500" />
              microfone aberto
            </span>
          ) : null}
          <Button
            variant="outline"
            size="sm"
            className="h-7 text-[11.5px]"
            onClick={onStop}
          >
            <MicOff className="size-3" aria-hidden />
            Encerrar
          </Button>
        </div>
      </div>

      {state.heard ? (
        <p className="rounded-lg border border-border/50 bg-background/40 px-3 py-2 text-[12.5px] leading-relaxed text-foreground/85">
          <span className="text-muted-foreground">Ouvi:</span> {state.heard}
        </p>
      ) : null}

      {state.error ? (
        <p
          role="alert"
          className="rounded-lg border border-destructive/25 bg-destructive/10 px-3 py-2 text-[12.5px] text-destructive"
        >
          {state.error}
        </p>
      ) : null}
    </section>
  );
}

const HINTS: Record<ConversationPhase, string> = {
  idle: "Pressione para começar.",
  listening: "Pode falar — pergunte ou conte algo para guardar.",
  hearing: "Estou ouvindo. Pare de falar quando terminar.",
  working: "Procurando nas suas memórias…",
  speaking: "Fale por cima para interromper.",
  follow_up: "Continue falando, ou espere para encerrar.",
};

/**
 * Four states, four looks. Reuses the sky→violet gradient that is the app's
 * identity rather than inventing a palette for this screen.
 */
function Orb({ phase }: { phase: ConversationPhase }) {
  const busy = phase === "working";
  const speaking = phase === "speaking";
  const hearing = phase === "hearing";

  return (
    <div
      className={cn(
        "relative flex size-9 shrink-0 items-center justify-center rounded-full transition-colors",
        hearing
          ? "bg-gradient-to-br from-sky-500 to-violet-500 text-white"
          : "border border-border/60 bg-background/60 text-muted-foreground",
      )}
      aria-hidden
    >
      {hearing ? (
        <span className="absolute inset-0 animate-ping rounded-full bg-sky-500/25" />
      ) : null}
      {busy ? (
        <Loader2 className="size-4 animate-spin" />
      ) : speaking ? (
        <Volume2 className="size-4 animate-pulse text-sky-500" />
      ) : (
        <Mic className="size-4" />
      )}
    </div>
  );
}
