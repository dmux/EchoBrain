"use client";

import { useEffect, useState } from "react";
import { Keyboard, X, Loader2, Search } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { KindBadge } from "@/components/kind-badge";
import { Waveform } from "@/components/waveform";
import { KINDS, type CaptureResult, type Kind } from "@/lib/memory";
import type { DictationPhase } from "@/lib/use-dictation";
import { cn } from "@/lib/utils";
import { playRecordStartSound } from "@/lib/sound";

export interface DictationControls {
  available: boolean;
  phase: DictationPhase;
  level: number;
  toggle: () => void;
}

/**
 * Capture and search, by voice or by keyboard.
 *
 * `text` is controlled from above so a transcript can land in it.
 *
 * # Why two buttons and not one
 *
 * Searching used to need no button: it happened on a timer as you typed, and
 * the only button was *Guardar*. Taking the timer away — it made the field feel
 * like it was thinking instead of typing — would have left no way at all to
 * search a sentence the router files as a statement. So both actions are now
 * explicit and both are always reachable.
 *
 * `primary` decides which one Enter runs and which one looks like the default.
 * It comes from the same cue router the voice path uses, so the key does what
 * the chip above the bar says it will.
 */
export function CaptureBar({
  text,
  onTextChange,
  onCapture,
  onSearch,
  primary,
  dictation,
  searching,
  onStartConversation,
}: {
  text: string;
  onTextChange: (text: string) => void;
  onCapture: (text: string, kind?: Kind) => Promise<CaptureResult | null>;
  onSearch: (text: string) => Promise<void>;
  /** Which action Enter runs. */
  primary: "search" | "capture";
  dictation: DictationControls;
  searching?: boolean;
  onStartConversation?: () => void;
}) {
  const [kind, setKind] = useState<Kind | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const [showText, setShowText] = useState(false);

  const recording = dictation.phase === "recording";
  const transcribing = dictation.phase === "transcribing";

  useEffect(() => {
    if (recording) {
      playRecordStartSound();
    }
  }, [recording]);

  const shouldShowText = showText || text.trim().length > 0;
  const isTextVisible = shouldShowText && !recording && !transcribing;

  const idle = !text.trim() || busy || recording || transcribing;

  async function store() {
    if (idle) return;
    setBusy(true);
    try {
      const result = await onCapture(text, kind);
      if (result) {
        onTextChange("");
        setKind(undefined);
      }
    } finally {
      setBusy(false);
    }
  }

  /**
   * Search, keeping the text in the field.
   *
   * Deliberately unlike `store`, which clears it: a capture is finished once it
   * is stored, while a search is the start of reading the results — and the
   * query has to stay visible for the cards underneath to mean anything.
   */
  async function look() {
    if (idle) return;
    setBusy(true);
    try {
      await onSearch(text);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="space-y-4">
      {recording || transcribing ? (
        <div className="relative animate-in slide-in-from-top-2 fade-in">
          {recording && (
            <div 
              className="absolute -inset-[2px] z-0 rounded-lg bg-gradient-to-r from-emerald-500 via-sky-500 to-emerald-500 opacity-60 blur-md duration-500"
              style={{
                backgroundSize: "200% 100%",
                animation: "gradient-shift 3s ease-in-out infinite"
              }}
            />
          )}
          <div className="relative z-10 flex items-center justify-center gap-4 rounded-lg border border-border/20 bg-background/90 px-4 py-5 backdrop-blur-md shadow-sm">
            <Waveform level={dictation.level} active={recording} />
            <span className="text-[13px] font-medium tabular-nums text-muted-foreground">
              {recording ? "ouvindo… ⌥Espaço para parar" : "transcrevendo…"}
            </span>
          </div>
        </div>
      ) : null}

      {!isTextVisible ? (
        <div className="flex flex-col items-center justify-center gap-4 py-2">
          {dictation.available || !!onStartConversation ? (
            <Button
              type="button"
              variant={recording ? "default" : "outline"}
              className={cn(
                "relative h-20 w-20 rounded-full shrink-0 shadow-sm transition-all hover:scale-105",
                recording && "bg-red-500 hover:bg-red-500/90 text-primary-foreground",
              )}
              onClick={() => {
                if (recording) {
                  dictation.toggle();
                } else if (onStartConversation) {
                  onStartConversation();
                } else {
                  dictation.toggle();
                }
              }}
              disabled={transcribing}
              aria-label={recording ? "Parar de gravar" : "Falar"}
              title="⌥Espaço"
            >
              {recording && (
                <span 
                  className="absolute inset-0 rounded-full bg-red-500 opacity-60"
                  style={{
                    transform: `scale(${1 + dictation.level * 0.8})`,
                    transition: 'transform 75ms cubic-bezier(0.4, 0, 0.2, 1)',
                  }}
                />
              )}
              <MicIcon className="size-8" />
            </Button>
          ) : (
            <p className="text-[13px] text-muted-foreground">Voz indisponível</p>
          )}
          
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => setShowText(true)}
            title="Digitar ou buscar"
            className="text-muted-foreground hover:text-foreground h-8 text-xs"
          >
            <Keyboard className="mr-2 size-3.5" />
            Teclado opcional
          </Button>
        </div>
      ) : (
        <div className="space-y-2 animate-in slide-in-from-bottom-2 fade-in">
          <div className="relative z-10 flex gap-2 rounded-lg bg-background p-0.5">
              {dictation.available ? (
                <Button
                  type="button"
                  variant={recording ? "default" : "outline"}
                  size="icon"
                  className={cn(
                    "relative h-10 w-10 shrink-0",
                    recording && "bg-red-500 hover:bg-red-500/90 text-primary-foreground",
                  )}
                  onClick={dictation.toggle}
                  disabled={transcribing}
                  aria-label={recording ? "Parar de gravar" : "Ditar"}
                  title="⌥Espaço"
                >
                  {recording && (
                    <span 
                      className="absolute inset-0 rounded-md bg-red-500 opacity-60"
                      style={{
                        transform: `scale(${1 + dictation.level * 0.8})`,
                        transition: 'transform 75ms cubic-bezier(0.4, 0, 0.2, 1)',
                      }}
                    />
                  )}
                  <MicIcon className="size-4" />
                </Button>
              ) : null}

              <div className="relative flex-1">
                <Input
                  value={text}
                  onChange={(event) => {
                    let newText = event.target.value;
                    const match = /^\/(\w+)\s/.exec(newText);
                    if (match) {
                      const k = match[1].toLowerCase();
                      if (KINDS.includes(k as Kind)) {
                        setKind(k as Kind);
                        newText = newText.substring(match[0].length);
                      }
                    }
                    onTextChange(newText);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && !event.nativeEvent.isComposing) {
                      event.preventDefault();
                      // ⌘↵ always searches, whatever the router guessed. The
                      // escape hatch matters because the guess is a heuristic
                      // and re-typing a sentence to look it up is absurd.
                      if (event.metaKey || event.ctrlKey) {
                        void look();
                      } else if (primary === "search") {
                        void look();
                      } else {
                        void store();
                      }
                    }
                  }}
                  placeholder="Busque memórias ou guarde algo novo..."
                  disabled={busy || recording || transcribing}
                  className="h-10 border-none bg-background/50 shadow-none pr-8 focus-visible:ring-0"
                  aria-label="Texto para busca ou captura"
                  autoFocus
                />
                {searching ? (
                  <span
                    className="absolute right-3 top-1/2 size-3 -translate-y-1/2 animate-spin rounded-full border-2 border-muted-foreground/25 border-t-muted-foreground/80"
                    aria-label="Buscando"
                  />
                ) : null}
              </div>

              <Button
                type="button"
                variant={primary === "search" ? "default" : "outline"}
                onClick={() => void look()}
                disabled={idle}
                className="h-10 shrink-0"
                title={primary === "search" ? "↵" : "⌘↵"}
              >
                {searching ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <>
                    <Search className="mr-1.5 size-3.5" />
                    Buscar
                  </>
                )}
              </Button>
              <Button
                type="button"
                variant={primary === "capture" ? "default" : "outline"}
                onClick={() => void store()}
                disabled={idle}
                className="h-10 shrink-0"
                title={primary === "capture" ? "↵" : undefined}
              >
                {busy && !searching ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  "Guardar"
                )}
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-10 w-10 shrink-0 text-muted-foreground"
                onClick={() => {
                  setShowText(false);
                  onTextChange("");
                }}
                title="Esconder teclado"
              >
                <X className="size-4" />
              </Button>
            </div>

          <div className="flex items-center gap-2 text-[11px] text-muted-foreground pl-12">
            <span>
              {kind ? "Classificado como" : "Classificação automática para novos registros:"}
            </span>
            {KINDS.map((candidate) => (
              <button
                key={candidate}
                type="button"
                onClick={() => setKind(kind === candidate ? undefined : candidate)}
                className={
                  kind === candidate
                    ? "opacity-100"
                    : "opacity-45 transition-opacity hover:opacity-80"
                }
                aria-pressed={kind === candidate}
              >
                <KindBadge kind={candidate} />
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function MicIcon({ className = "size-4" }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      className={className}
      aria-hidden
    >
      <path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z" />
      <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
      <path d="M12 19v3" />
    </svg>
  );
}
