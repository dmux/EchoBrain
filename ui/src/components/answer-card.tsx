"use client";

import { Loader2, Quote, Volume2, VolumeX } from "lucide-react";

import { HitCard } from "@/components/hit-card";
import { MarkdownContent } from "@/components/markdown-content";
import { Button } from "@/components/ui/button";
import type { Answer, MemoryGateway } from "@/lib/memory";

/**
 * A composed answer, and — always — the memories the model was given.
 *
 * # Why the list says "read", not "came from"
 *
 * Retrieval hands over everything above the similarity floor, and the floor is
 * a statement about vectors rather than about relevance. A real run answered
 * from one memory at 68% while four unrelated ones — branch rules, test
 * coverage — sat behind it at 57–60%. Labelling those as the source would be
 * false for four of the five, and a list that is wrong most of the time is a
 * list people learn to skip.
 *
 * So the heading says what is true — this is what the model read — and the
 * similarity meter on each card shows which one it can plausibly have used.
 *
 * # Why the sources are not collapsible
 *
 * Every other surface in EchoBrain plays a memory back verbatim: what returns
 * is what was said, and a wrong result is a wrong *search*, which the
 * similarity meter makes visible. This one is different. A model wrote this
 * sentence, it can be fluent and unsupported, and when it arrives by voice
 * there is nothing to skim.
 *
 * So the sources render underneath, expanded, every time. Putting them behind a
 * disclosure would make checking the answer an action the user has to choose,
 * and almost nobody chooses it. That is the whole point of showing them.
 */
export function AnswerCard({
  answer,
  pending,
  speaking,
  onSpeak,
  onStopSpeaking,
  canSpeak,
  gateway,
}: {
  answer: Answer | null;
  pending: boolean;
  speaking: boolean;
  onSpeak: () => void;
  onStopSpeaking: () => void;
  canSpeak: boolean;
  gateway: MemoryGateway;
}) {
  if (pending) {
    return (
      <div
        className="flex items-center gap-2.5 rounded-xl border border-border/60 bg-card/60 px-4 py-3.5 backdrop-blur-sm"
        aria-live="polite"
      >
        <Loader2
          className="size-3.5 animate-spin text-muted-foreground"
          aria-hidden
        />
        <span className="text-[13px] text-muted-foreground">
          Procurando nas suas memórias…
        </span>
      </div>
    );
  }

  if (!answer) return null;

  return (
    <section className="animate-in fade-in slide-in-from-bottom-2 space-y-3">
      <article
        className={
          answer.grounded
            ? "rounded-xl border border-sky-500/25 bg-gradient-to-br from-sky-500/[0.07] to-violet-500/[0.07] p-4 backdrop-blur-sm"
            : "rounded-xl border border-dashed border-border/60 bg-card/40 p-4 backdrop-blur-sm"
        }
        aria-live="polite"
      >
        <div className="flex items-start gap-3">
          <Quote
            className="mt-0.5 size-4 shrink-0 text-sky-500/70"
            aria-hidden
          />
          <div className="flex-1">
            <MarkdownContent content={answer.text} />
          </div>
          {canSpeak ? (
            <Button
              variant="ghost"
              size="icon"
              className="size-7 shrink-0 text-muted-foreground hover:text-foreground"
              onClick={speaking ? onStopSpeaking : onSpeak}
              aria-label={speaking ? "Parar a leitura" : "Ouvir a resposta"}
            >
              {speaking ? (
                <VolumeX className="size-3.5" />
              ) : (
                <Volume2 className="size-3.5" />
              )}
            </Button>
          ) : null}
        </div>

        <p className="mt-3 border-t border-border/40 pt-2.5 text-[11px] text-muted-foreground/80">
          {answer.grounded ? (
            <>
              Escrito por um modelo, que leu{" "}
              {answer.cited.length === 1
                ? "a memória abaixo"
                : `as ${answer.cited.length} memórias abaixo`}
              . Confira antes de confiar.
            </>
          ) : answer.consulted ? (
            // The memories cleared the floor; the model read them and said they
            // do not answer this. Different from finding nothing, and worth
            // saying so: it means the question was understood and the store
            // genuinely has no answer, not that the search missed.
            <>O modelo leu as memórias mais próximas e nenhuma responde a isso.</>
          ) : (
            <>Nada chegou perto o bastante — nenhum modelo foi consultado.</>
          )}
        </p>
      </article>

      {answer.cited.length > 0 ? (
        <div className="space-y-2.5">
          <h2 className="px-0.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground/70">
            O que o modelo leu
          </h2>
          <p className="px-0.5 text-[11px] leading-relaxed text-muted-foreground/70">
            Tudo que passou do piso de semelhança, da mais próxima para a menos.
            Nem todas foram usadas — a busca é por vetor, e uma memória pode
            ficar perto sem ter nada a ver com a pergunta.
          </p>
          {answer.cited.map((hit, index) => (
            <HitCard key={hit.memory.memoryId} hit={hit} index={index} gateway={gateway} />
          ))}
        </div>
      ) : null}
    </section>
  );
}
