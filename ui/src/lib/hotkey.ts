import { isTauri } from "@/lib/memory";

/** Must match `TOGGLE_EVENT` in `crates/echobrain-app/src/hotkey.rs`. */
const TOGGLE_EVENT = "echobrain://toggle-dictation";

/**
 * Subscribe to the ⌥Space global shortcut.
 *
 * The Rust side only raises the window and emits; deciding what the toggle
 * *means* stays here, because only this side knows whether a dictation is
 * already running. Returns an unsubscribe function.
 *
 * In a browser there is no Tauri event bus, so this attaches a plain keydown
 * listener instead — the same chord works in `make ui`, and the dictation flow
 * can be designed against `FakeGateway` without building the app.
 */
export function onToggleDictation(handler: () => void): () => void {
  if (!isTauri()) {
    const onKey = (event: KeyboardEvent) => {
      if (event.altKey && event.code === "Space") {
        event.preventDefault();
        handler();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }

  let dispose: (() => void) | null = null;
  let cancelled = false;

  // Imported lazily so the module graph stays loadable in a plain browser,
  // where `@tauri-apps/api/event` has nothing to talk to.
  void import("@tauri-apps/api/event").then(({ listen }) =>
    listen(TOGGLE_EVENT, () => handler()).then((unlisten) => {
      if (cancelled) unlisten();
      else dispose = unlisten;
    }),
  );

  return () => {
    cancelled = true;
    dispose?.();
  };
}
