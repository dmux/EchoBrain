import { isTauri, type ConversationState } from "@/lib/memory";

/** Must match `STATE_EVENT` in `crates/echobrain-app/src/conversation.rs`. */
const STATE_EVENT = "echobrain://conversation-state";

/**
 * Subscribe to the conversation's phase.
 *
 * Pushed rather than polled because the transitions are driven by audio: the
 * detector decides when someone started and stopped, and the UI has no way to
 * ask. Returns an unsubscribe function.
 *
 * In a browser this never fires. That is deliberate — `FakeGateway` reports
 * conversation mode as unavailable, because a fake that scripted its own phases
 * would let the UI be designed against timings the real detector does not
 * produce.
 */
export function onConversationState(
  handler: (state: ConversationState) => void,
): () => void {
  if (!isTauri()) return () => {};

  let dispose: (() => void) | null = null;
  let cancelled = false;

  // Imported lazily so the module graph stays loadable in a plain browser,
  // matching `hotkey.ts`.
  void import("@tauri-apps/api/event").then(({ listen }) =>
    listen<ConversationState>(STATE_EVENT, (event) =>
      handler(event.payload),
    ).then((unlisten) => {
      if (cancelled) unlisten();
      else dispose = unlisten;
    }),
  );

  return () => {
    cancelled = true;
    dispose?.();
  };
}
