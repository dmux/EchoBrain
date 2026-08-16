"use client";

import { HelpCircle, Save } from "lucide-react";

import { cn } from "@/lib/utils";
import { INTENT_LABELS, type Intent } from "@/lib/memory";

/**
 * What the app is about to do with an utterance, and a way to say otherwise.
 *
 * The routing is a cue-phrase heuristic (`CueIntent` in `echobrain-core`), so
 * it is wrong sometimes. It gets the same treatment the memory kind gets — a
 * visible, one-click-correctable guess — rather than being applied silently,
 * because the two mistakes it can make are not recoverable in the same way: a
 * question stored as a memory is visible and deletable, but a memory routed to
 * a search is simply thrown away.
 */
export function IntentChip({
  intent,
  onChange,
  disabled,
}: {
  intent: Intent;
  onChange: (intent: Intent) => void;
  disabled?: boolean;
}) {
  return (
    <div
      className="inline-flex items-center gap-0.5 rounded-full border border-border/60 bg-muted/30 p-0.5"
      role="radiogroup"
      aria-label="O que fazer com este texto"
    >
      <Option
        value="record"
        current={intent}
        onChange={onChange}
        disabled={disabled}
        icon={<Save className="size-3" aria-hidden />}
      />
      <Option
        value="ask"
        current={intent}
        onChange={onChange}
        disabled={disabled}
        icon={<HelpCircle className="size-3" aria-hidden />}
      />
    </div>
  );
}

function Option({
  value,
  current,
  onChange,
  disabled,
  icon,
}: {
  value: Intent;
  current: Intent;
  onChange: (intent: Intent) => void;
  disabled?: boolean;
  icon: React.ReactNode;
}) {
  const selected = current === value;

  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      disabled={disabled}
      onClick={() => onChange(value)}
      className={cn(
        "flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium transition-colors",
        "disabled:cursor-not-allowed disabled:opacity-50",
        selected
          ? value === "ask"
            ? "bg-sky-500/15 text-sky-600 dark:text-sky-300"
            : "bg-violet-500/15 text-violet-600 dark:text-violet-300"
          : "text-muted-foreground hover:text-foreground",
      )}
    >
      {icon}
      {INTENT_LABELS[value]}
    </button>
  );
}
