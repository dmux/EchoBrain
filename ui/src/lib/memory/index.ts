import { FakeGateway } from "./fake";
import type { MemoryGateway } from "./gateway";
import { TauriGateway } from "./tauri";

export type { MemoryGateway } from "./gateway";
export * from "./types";
export { FakeGateway } from "./fake";
export { TauriGateway } from "./tauri";

/**
 * Whether the page is running inside a Tauri window rather than a browser.
 *
 * Tauri v2 injects `__TAURI_INTERNALS__` before any application script runs, so
 * this is settled by the time a component asks. Checked at runtime rather than
 * at build time on purpose: the very same static bundle is served by `next dev`
 * during UI work and embedded in the macOS app, and it has to
 * behave correctly in both without a rebuild.
 */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

let cached: MemoryGateway | null = null;

/**
 * The gateway this environment should use.
 *
 * Memoised because {@link FakeGateway} holds state — captures made during a
 * `next dev` session should survive a re-render, the way real ones would.
 */
export function resolveGateway(): MemoryGateway {
  if (!cached) {
    cached = isTauri() ? new TauriGateway() : new FakeGateway();
  }
  return cached;
}
