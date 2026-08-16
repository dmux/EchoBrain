"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Brain,
  Library as LibraryIcon,
  Maximize,
  Minimize,
  Waypoints,
} from "lucide-react";

import { AnswerCard } from "@/components/answer-card";
import { CaptureBar } from "@/components/capture-bar";
import { SettingsDialog } from "@/components/settings-dialog";
import { ConversationView } from "@/components/conversation-view";
import { onConversationState } from "@/lib/conversation";
import { IntentChip } from "@/components/intent-chip";
import { useDictation } from "@/lib/use-dictation";
import { onToggleDictation } from "@/lib/hotkey";
import { HitCard } from "@/components/hit-card";
import { KindBadge } from "@/components/kind-badge";
import { Skeleton } from "@/components/ui/skeleton";
import { LibraryView } from "@/components/library-view";
import { GraphView } from "@/components/graph-view";
import { plainText } from "@/lib/plain-text";
import { cn } from "@/lib/utils";

import {
  type Answer,
  type CaptureResult,
  type ConversationState,
  type Hit,
  type Intent,
  isTauri,
  type Kind,
  KIND_LABELS,
  MemoryGatewayError,
  resolveGateway,
} from "@/lib/memory";

/**
 * Debounce before *classifying* what was typed.
 *
 * The only thing left on a timer, and it is the cheap one: `classify_intent` is
 * a cue-phrase match in Rust — no network, no model, no token — so it can run
 * while you type in order to label the buttons and show the chip.
 *
 * Searching used to be on a timer too. It is not any more: a search is an
 * embedding plus a vector search across the Atlantic, and firing one on every
 * pause made the field feel like it was thinking instead of typing. Now nothing
 * leaves the machine until you press *Buscar* — see `runSearch`.
 */
const CLASSIFY_DEBOUNCE_MS = 280;

export function EchoPanel() {
  const gateway = useMemo(() => resolveGateway(), []);


  const [view, setView] = useState<"search" | "library" | "graph">("search");
  const [notice, setNotice] = useState<string | null>(null);
  const [captureError, setCaptureError] = useState<string | null>(null);

  const capture = useCallback(
    async (text: string, kind?: Kind): Promise<CaptureResult | null> => {
      try {
        const result = await gateway.capture(text, { kind });
        setCaptureError(null);
        setNotice(
          result.journalFailed
            ? `Guardado, mas o registro local falhou: ${result.journalFailed}`
            : `Guardado como ${KIND_LABELS[result.entry.kind]}. Buscável em até ~90 s.`,
        );
        return result;
      } catch (raw) {
        setCaptureError(messageOf(raw));
        return null;
      }
    },
    [gateway],
  );

  // Lifted out of CaptureBar so a transcript can land in it.
  const [draft, setDraft] = useState("");

  /**
   * What the app decided to do with an utterance, held together with the text
   * that produced the decision.
   *
   * Same shape as `settled` below, for the same reason: a routing decision for
   * text the user has since edited is stale, and pairing it with its query lets
   * the chip *derive* that instead of needing an effect to clear it. Typing used
   * to wipe this on every keystroke, which is why a typed question could never
   * reach the answering path at all.
   */
  const [routed, setRouted] = useState<{
    query: string;
    intent: Intent;
    /** Dictated rather than typed — the only thing that decides speaking. */
    spoken: boolean;
  } | null>(null);

  /**
   * The question the model has already been paid for.
   *
   * A ref, read synchronously inside `runSearch` before any await, so a second
   * click cannot bill the same sentence twice while the first is still in
   * flight — a value that only becomes current on the next render would be too
   * late for that.
   */
  const asked = useRef<string | null>(null);

  /**
   * The same string, as state, purely so the answer can say what it answered.
   *
   * The ref is the guard and this is the label; they are set together. A ref
   * alone cannot do this job because changing it renders nothing.
   */
  const [askedQuestion, setAskedQuestion] = useState<string | null>(null);

  const [answer, setAnswer] = useState<Answer | null>(null);
  const [asking, setAsking] = useState(false);
  const [canAnswer, setCanAnswer] = useState(false);
  const [conversation, setConversation] = useState<ConversationState | null>(
    null,
  );
  const [canSpeak, setCanSpeak] = useState(false);
  const [speaking, setSpeaking] = useState(false);
  const [canConverse, setCanConverse] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void Promise.all([
      gateway.answeringAvailable().catch(() => false),
      gateway.speechAvailable().catch(() => false),
      gateway.conversationAvailable().catch(() => false),
    ]).then(([answering, speech, converse]) => {
      if (cancelled) return;
      setCanAnswer(answering);
      setCanSpeak(speech);
      setCanConverse(converse);
    });
    return () => {
      cancelled = true;
    };
  }, [gateway]);

  /**
   * Ask, and read the answer aloud when the question arrived by voice.
   *
   * Typed questions land here too, but silently: `speak` is the flag the Rust
   * side also uses to choose between the spoken prompt (three sentences, no
   * markdown, because there is no skimming a voice) and the written one. The
   * frontend is the only side that knows which it was.
   */
  const ask = useCallback(
    async (question: string, speak: boolean) => {
      // Claimed before the first await, so a re-render mid-flight cannot start
      // a second billed request for the same sentence.
      asked.current = question;
      setAskedQuestion(question);
      setAsking(true);
      setAnswer(null);
      setCaptureError(null);
      try {
        if (speak) setSpeaking(true);
        const composed = await gateway.ask(question, { speak });
        setAnswer(composed);
      } catch (raw) {
        setCaptureError(messageOf(raw));
      } finally {
        setAsking(false);
        setSpeaking(false);
      }
    },
    [gateway],
  );

  const dictation = useDictation(gateway, async (text) => {
    const spoken = text.trim();
    if (!spoken) return;

    setDraft(spoken);

    // The router is a heuristic and shows itself as an editable chip, but it
    // still acts: a voice interface that waits for a click is not one.
    const decision = canAnswer
      ? await gateway.classifyIntent(spoken).catch(() => "record" as Intent)
      : "record";
    setRouted({ query: spoken, intent: decision, spoken: true });

    if (decision === "ask") {
      await ask(spoken, canSpeak);
      setDraft("");
      return;
    }

    await capture(spoken);
    setTimeout(() => {
      setDraft((current) => (current === spoken ? "" : current));
    }, 4000);
  });

  /**
   * Re-route an utterance the classifier got wrong, without retyping it.
   *
   * # Why the correction only *acts* on dictated text
   *
   * When the sentence arrived by voice, acting is the whole point: the user's
   * hands were not on the keyboard, and a voice interface that waits for a click
   * is not one — so correcting the chip runs the thing it was corrected to.
   *
   * When the sentence was typed, the buttons are right there, and this app no
   * longer does anything expensive without being told to. Flipping the chip
   * therefore only re-labels them — it changes which one Enter runs — and the
   * user presses it. Acting here would put a model call behind a chip click,
   * which is exactly the surprise that taking the search off the keystroke timer
   * was meant to remove.
   */
  const reroute = useCallback(
    async (next: Intent) => {
      const text = draft.trim();
      // Correcting the chip does not change how the sentence arrived, so the
      // answer is spoken only if the question was.
      const wasSpoken = routed?.query === text && routed.spoken;
      setRouted(text ? { query: text, intent: next, spoken: wasSpoken } : null);
      if (!text || !wasSpoken) return;

      if (next === "ask") {
        await ask(text, canSpeak);
        setDraft("");
      } else {
        setAnswer(null);
        await capture(text);
        setDraft("");
      }
    },
    [ask, canSpeak, capture, draft, routed],
  );

  /**
   * Results, held together with the query that produced them.
   *
   * The pairing survived the move to an explicit search, but it now answers a
   * different question. It used to derive *"is a search in flight"* by comparing
   * the settled query with the live one — which only worked because every
   * keystroke started one. Now it tells the user which text these cards are
   * actually about, so editing the field does not silently relabel results that
   * came from something else.
   */
  const [settled, setSettled] = useState<{
    query: string;
    hits: Hit[];
    error: string | null;
  }>({ query: "", hits: [], error: null });

  /**
   * A search is in flight.
   *
   * Real state now rather than a derivation, because there is nothing left to
   * derive it from: with no timer firing on every keystroke, a draft that
   * differs from the settled query means the user is typing, not waiting. It is
   * only ever set from an event handler, so the rule about `setState` in effects
   * is not in play.
   */
  const [searching, setSearching] = useState(false);

  const trimmed = draft.trim();
  const hits = settled.hits;
  const error = captureError ?? dictation.error ?? settled.error;

  /** The decision describes exactly what is on screen — nothing to re-check. */
  const routingSettled = routed?.query === trimmed;

  /**
   * The routing decision, kept across edits rather than blanked between them.
   *
   * # Why this is not `routed.query === trimmed`
   *
   * It was, and that is what made the panel flicker while you typed: every
   * keystroke made the decision "stale", the chip vanished and the buttons swapped
   * variants, then 280 ms later the classifier answered and all of it came back.
   * Once per character.
   *
   * Holding the last decision is also the *more correct* answer, not just the
   * calmer one. The cues this router looks for sit at the **start** of a sentence
   * — "por que…", "quais…", "grava…" — so a verdict reached on `"quais as
   * mudanç"` is still the right verdict for `"quais as mudanças na paginação"`.
   * Throwing it away mid-word discarded a good answer to redraw the same one.
   *
   * `routingSettled` above keeps the strictness where it is actually needed: it
   * is what tells the effect there is something new to classify.
   */
  const intent = trimmed ? routed?.intent : undefined;

  /**
   * Search, on purpose, when asked.
   *
   * # Why this is not on a timer any more
   *
   * It was, at 280 ms, and the reasoning at the time was about *cost* — don't
   * multiply the bill by the length of the query. That was true and beside the
   * point. The thing that made it unusable is latency: a search is an embedding
   * plus a vector search over the network, so every pause longer than a quarter
   * second put a spinner in the field and cards on screen while the sentence was
   * still half-written. Typing felt like waiting.
   *
   * So the whole trip is now one gesture. And because the same gesture already
   * knows whether the sentence was a question, answering rides along with it:
   * pressing *Buscar* on *"por que não fomos de OpenSearch?"* returns the cards
   * **and** the answer, from one decision the user made deliberately.
   */
  const runSearch = useCallback(
    async (text: string) => {
      const query = text.trim();
      if (!query || searching) return;

      setSearching(true);
      setCaptureError(null);
      try {
        const found = await gateway.search(query, { topK: 8 });
        setSettled({ query, hits: found, error: null });
      } catch (raw) {
        setSettled({ query, hits: [], error: messageOf(raw) });
      } finally {
        setSearching(false);
      }

      // Typed, so never spoken. The answer is composed in markdown and rendered
      // under the field; see `AnswerStyle` on the Rust side.
      if (intent === "ask" && asked.current !== query) {
        await ask(query, false);
      }
    },
    [ask, gateway, intent, searching],
  );

  /**
   * Route typed text the same way dictated text is routed.
   *
   * The one thing still allowed to run while you type, because it is the one
   * thing that costs nothing: `classify_intent` is a cue-phrase match in Rust,
   * no network and no model. It earns its place by labelling the buttons — the
   * bar can say *Buscar* or *Guardar* before you commit to either — and by
   * putting the chip on screen so a wrong guess is corrected before it acts,
   * rather than after.
   */
  useEffect(() => {
    if (!trimmed || !canAnswer) return;
    // Already decided for this exact text — by the dictation path, or by a
    // correction the user made on the chip. Re-classifying would overwrite both
    // with a fresh guess, and in the dictation case would also forget that the
    // question was spoken, silencing the answer to a spoken question.
    if (routingSettled) return;

    const timer = setTimeout(async () => {
      const decision = await gateway
        .classifyIntent(trimmed)
        .catch(() => "record" as Intent);
      // The object is always replaced — `query` has to move forward or this
      // effect never settles. That is fine: what the user sees is derived from
      // `intent`, and re-rendering the same verdict produces the same DOM, so
      // React changes nothing on screen.
      setRouted({ query: trimmed, intent: decision, spoken: false });
    }, CLASSIFY_DEBOUNCE_MS);

    return () => clearTimeout(timer);
  }, [trimmed, canAnswer, gateway, routingSettled]);

  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(() => setNotice(null), 6000);
    return () => clearTimeout(timer);
  }, [notice]);

  /**
   * Barge-in: reaching for the microphone silences the answer in progress.
   *
   * The authoritative half of this lives in `start_dictation` on the Rust side,
   * which stops the speaker whatever opened the microphone. This half only
   * catches the button up with it, and it is an event handler rather than an
   * effect on `phase` — the panel deliberately keeps setState out of effects.
   */
  const toggleDictation = useCallback(() => {
    setSpeaking(false);
    dictation.toggle();
  }, [dictation]);

  useEffect(() => onToggleDictation(toggleDictation), [toggleDictation]);

  // Tray menu navigation: "Buscar Memória" and "Biblioteca" emit this event.
  useEffect(() => {
    if (!isTauri()) return;
    let dispose: (() => void) | null = null;
    let cancelled = false;
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen<string>("echobrain://navigate", (event) => {
        if (event.payload === "library") setView("library");
        else if (event.payload === "search") setView("search");
      }).then((unlisten) => {
        if (cancelled) unlisten();
        else dispose = unlisten;
      }),
    );
    return () => {
      cancelled = true;
      dispose?.();
    };
  }, []);

  /**
   * The conversation's phase, pushed from Rust.
   *
   * An event subscription rather than polling: the transitions are driven by
   * the detector hearing silence, and there is nothing to ask.
   */
  const [widescreen, setWidescreen] = useState(false);

  useEffect(
    () =>
      onConversationState((next) =>
        setConversation(next.phase === "idle" ? null : next),
      ),
    [],
  );

  /**
   * How the result cards are laid out.
   *
   * A stack at normal width; a grid once the panel fills the window, because a
   * single column of cards 1900px wide is not "using the space", it is one very
   * wide card. Columns are what a big display actually buys a result list.
   */
  const cards = widescreen
    ? "grid gap-2.5 md:grid-cols-2 2xl:grid-cols-3 items-start"
    : "space-y-2.5";

  /**
   * Prose stays in a column you can read.
   *
   * Everything else grows with the window; this does not. A composed answer and
   * the field you type into are both continuous text, and a line that runs the
   * width of a large monitor loses the reader on the way back to the left edge.
   */
  const reading = widescreen ? "mx-auto w-full max-w-3xl" : "";

  return (
    /*
      Widescreen means *the window*, not a wider column.

      It used to swap `max-w-2xl` for `max-w-5xl`, which is still a fixed 1024px
      strip with empty gutters either side — on a large display the button
      visibly did almost nothing. `max-w-none` lets the panel take the window it
      was given; the padding grows with it so the text does not run into the
      window edge, and the reading column below keeps its own limit, because a
      1900px-wide line of prose is unreadable no matter how much room there is.
    */
    <div
      className={cn(
        "mx-auto flex w-full flex-1 flex-col gap-5 pb-8 relative transition-all duration-300",
        widescreen ? "max-w-none px-6 lg:px-10" : "max-w-2xl px-5",
      )}
    >
      <header
        className={cn(
          "sticky top-0 z-20 mb-2 space-y-1 border-b border-border/10 bg-background/70 pb-4 pt-8 backdrop-blur-xl",
          widescreen ? "-mx-6 px-6 lg:-mx-10 lg:px-10" : "-mx-5 px-5",
        )}
      >
        <div className="flex items-baseline justify-between">
          <h1 className="flex items-center gap-2 text-lg font-semibold tracking-tight">
            <Brain className="size-6 text-pink-500" />
            EchoBrain
          </h1>
          <div className="flex items-center gap-1.5">
            <button
              onClick={() => setWidescreen(!widescreen)}
              className="mr-2 flex items-center gap-1 text-[11px] font-medium text-muted-foreground hover:text-foreground"
              title={widescreen ? "Modo normal" : "Modo foco (widescreen)"}
            >
              {widescreen ? <Minimize className="size-3" /> : <Maximize className="size-3" />}
            </button>
            {/*
              A labelled button rather than the bare 11px word it used to be.
              This is the only way into editing, rating and deactivating
              memories, and as plain text among the icon toggles it read as a
              caption — the feature was reported missing while sitting here.
            */}
            {/*
              Icon only, unlike the Biblioteca button next to it. That one is
              labelled because it is the only way into editing and rating, and
              was reported missing as a bare word among icons. This one leads
              nowhere exclusive — everything it shows is reachable from search —
              so it can afford to be quiet.
            */}
            <button
              onClick={() => setView(view === "graph" ? "search" : "graph")}
              className={cn(
                "mr-2 flex items-center rounded-md border p-1 transition-colors",
                view === "graph"
                  ? "border-sky-500/30 bg-sky-500/10 text-sky-500"
                  : "border-border/60 text-muted-foreground hover:border-border hover:text-foreground",
              )}
              title="Grafo: memórias ligadas por significado"
              aria-label="Grafo de memórias"
              aria-pressed={view === "graph"}
            >
              <Waypoints className="size-3.5" />
            </button>
            <button
              onClick={() => setView(view === "search" ? "library" : "search")}
              className={cn(
                "mr-2 flex items-center gap-1.5 rounded-md border px-2 py-1 text-[11px] font-medium transition-colors",
                view === "library"
                  ? "border-pink-500/30 bg-pink-500/10 text-pink-500"
                  : "border-border/60 text-muted-foreground hover:border-border hover:text-foreground"
              )}
              title="Ver, editar, avaliar e desativar memórias"
            >
              <LibraryIcon className="size-3" />
              {view === "search" ? "Biblioteca" : "Voltar"}
            </button>
            {/*
              The SSO button used to sit here. It moved into the settings
              dialog, next to the profile picker: logging in is now *per
              profile*, and a button that authenticates one account while the
              screen showing which account is in use lives elsewhere is a
              button you cannot reason about.
            */}
            <SettingsDialog gateway={gateway} />
            {!isTauri() ? (
              <span className="rounded-full border border-amber-500/25 bg-amber-500/10 px-2 py-0.5 font-mono text-[10px] text-amber-600 dark:text-amber-300">
                dados de exemplo
              </span>
            ) : null}
          </div>
        </div>
        <p className="text-[13px] text-muted-foreground">
          {canAnswer
            ? "Busca por significado. Pergunte — falando ou escrevendo — e ela responde."
            : "Busca por significado, não por palavra exata."}
        </p>
      </header>



      {error ? (
        <p
          role="alert"
          className="rounded-lg border border-destructive/25 bg-destructive/10 px-3 py-2 text-[13px] text-destructive"
        >
          {error}
        </p>
      ) : null}

      {notice ? (
        <p className="animate-in fade-in rounded-lg border border-border/60 bg-muted/40 px-3 py-2 text-[13px] text-muted-foreground">
          {notice}
        </p>
      ) : null}

      {view === "graph" ? (
        <section className="flex min-h-0 flex-1 flex-col pb-4">
          <GraphView gateway={gateway} />
        </section>
      ) : view === "library" ? (
        <section className="flex-1 overflow-y-auto pb-4" aria-live="polite">
          <LibraryView gateway={gateway} />
        </section>
      ) : (
        <>
          <section className="flex-1 space-y-4" aria-live="polite">
            {conversation ? (
              <ConversationView
                state={conversation}
                gateway={gateway}
                onStop={() => {
                  void gateway.stopConversation();
                  setConversation(null);
                }}
              />
            ) : null}

            {asking || answer ? (
              <div className={cn(reading, "space-y-1.5")}>
                {/*
                  Always rendered, never conditional on divergence: a line that
                  appears the moment you touch the keyboard is another thing
                  moving under the cursor. Saying which question this answers is
                  useful even when it is the one still in the field.
                */}
                {askedQuestion ? (
                  <p className="truncate text-[11px] text-muted-foreground/70">
                    Resposta para «{askedQuestion}»
                  </p>
                ) : null}
                <AnswerCard
                  answer={answer}
                  pending={asking}
                  speaking={speaking}
                  canSpeak={canSpeak}
                  onSpeak={() => {
                    if (!answer) return;
                    setSpeaking(true);
                    // A typed question gets a markdown answer, and a voice reads
                    // markdown literally. Stripped here rather than in `speak`,
                    // which serves callers whose text never was markdown.
                    void gateway
                      .speak(plainText(answer.text))
                      .finally(() => setSpeaking(false));
                  }}
                  onStopSpeaking={() => {
                    void gateway.stopSpeaking();
                    setSpeaking(false);
                  }}
                  gateway={gateway}
                />
              </div>
            ) : null}

            {searching && hits.length === 0 ? (
              <div className={cards}>
                {[0, 1, 2].map((row) => (
                  <Skeleton key={row} className="h-24 w-full rounded-xl" />
                ))}
              </div>
            ) : hits.length > 0 ? (
              <div className="space-y-2.5">
                {/*
                  Results outlive the text that produced them now that searching
                  is deliberate — clearing them on the next keystroke would throw
                  away what the user just paid for. So the cards say what they
                  are about instead of quietly appearing to answer whatever is in
                  the field.

                  Unconditional, for the same reason as the line above the
                  answer: showing it only once the text diverges would make it
                  appear on the first keystroke and push every card down a row.
                */}
                <p className="truncate text-[11px] text-muted-foreground/70">
                  Resultados de «{settled.query}»
                </p>
                <div className={cards}>
                  {hits.map((hit, index) => (
                    <HitCard key={hit.memory.memoryId} hit={hit} index={index} gateway={gateway} />
                  ))}
                </div>
              </div>
            ) : settled.query ? (
              <EmptyState
                title="Nada parecido."
                body="A busca é semântica: tente descrever a ideia em vez de acertar as palavras."
              />
            ) : answer || asking ? null : (
              <EmptyState
                title="Três tipos de memória."
                body="Guarde algo abaixo e depois procure pelo sentido — não pelas palavras que você usou."
                footer={
                  <div className="mt-3 flex gap-1.5">
                    <KindBadge kind="fact" />
                    <KindBadge kind="preference" />
                    <KindBadge kind="episode" />
                  </div>
                }
              />
            )}
          </section>

          {!conversation && (
            <footer
              className={cn(
                "space-y-2 border-t border-border/50 pt-4",
                reading,
              )}
            >
              {/*
                The row is reserved, not conditional.

                Rendering it only once a verdict exists meant the first
                classification — 280 ms after the first keystroke — pushed the
                whole capture bar down by its height, under the user's cursor.
                A fixed `h-6` holds the space open from the moment there is text,
                so the chip fades in without moving anything.
              */}
              {trimmed && canAnswer ? (
                <div className="flex h-6 items-center gap-2 text-[11px] text-muted-foreground">
                  {intent ? (
                    <>
                      <span>Entendi como</span>
                      <IntentChip
                        intent={intent}
                        onChange={(next) => void reroute(next)}
                        disabled={asking}
                      />
                    </>
                  ) : null}
                </div>
              ) : null}
              <CaptureBar
                text={draft}
                // Nothing but the draft. Editing used to also clear the answer,
                // which yanked a tall card out of the middle of the page on the
                // first keystroke and shoved everything up. The concern behind
                // it was real — an answer must not appear to answer whatever is
                // in the field now — and it is handled by naming the question
                // above the card instead of deleting the card.
                onTextChange={setDraft}
                onCapture={capture}
                onSearch={runSearch}
                primary={intent === "ask" ? "search" : "capture"}
                dictation={{ ...dictation, toggle: toggleDictation }}
                searching={searching}
                onStartConversation={canConverse ? () => {
                  void gateway.startConversation().catch((raw) => {
                    setCaptureError(messageOf(raw));
                  });
                } : undefined}
              />
            </footer>
          )}
        </>
      )}
    </div>
  );
}

function EmptyState({
  title,
  body,
  footer,
}: {
  title: string;
  body: string;
  footer?: React.ReactNode;
}) {
  return (
    <div className="flex h-full min-h-40 flex-col items-center justify-center rounded-xl border border-dashed border-border/50 px-6 py-10 text-center">
      <p className="text-[13.5px] font-medium">{title}</p>
      <p className="mt-1 max-w-sm text-[12.5px] leading-relaxed text-muted-foreground">
        {body}
      </p>
      {footer}
    </div>
  );
}

function messageOf(raw: unknown): string {
  if (raw instanceof MemoryGatewayError) return raw.message;
  return raw instanceof Error ? raw.message : String(raw);
}
