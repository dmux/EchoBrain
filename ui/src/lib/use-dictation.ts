"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { type MemoryGateway, MemoryGatewayError } from "@/lib/memory";

/** How often the input level is polled while recording. ~30 fps. */
const LEVEL_POLL_MS = 33;

export type DictationPhase = "idle" | "recording" | "transcribing";

/**
 * The dictation state machine, kept out of the panel component.
 *
 * `transcribing` is a real phase rather than a spinner on `recording`: whisper
 * takes a second or two even on Metal, and a UI that claims to still be
 * listening during it invites the user to keep talking into a closed microphone.
 */
export function useDictation(
  gateway: MemoryGateway,
  onTranscript: (text: string) => void,
) {
  const [available, setAvailable] = useState(false);
  const [phase, setPhase] = useState<DictationPhase>("idle");
  const [level, setLevel] = useState(0);
  const [error, setError] = useState<string | null>(null);

  // Read by the hotkey handler, which is registered once and would otherwise
  // capture a stale phase. Synced in an effect rather than during render:
  // writing a ref while rendering is the thing that makes a component miss an
  // update, and React lints for it.
  const phaseRef = useRef<DictationPhase>("idle");
  useEffect(() => {
    phaseRef.current = phase;
  }, [phase]);

  useEffect(() => {
    let cancelled = false;
    gateway
      .dictationAvailable()
      .then((yes) => !cancelled && setAvailable(yes))
      .catch(() => !cancelled && setAvailable(false));
    return () => {
      cancelled = true;
    };
  }, [gateway]);

  useEffect(() => {
    if (phase !== "recording") return;
    const timer = setInterval(async () => {
      try {
        setLevel(await gateway.audioLevel());
      } catch {
        // A failing level poll is not worth interrupting a recording over.
      }
    }, LEVEL_POLL_MS);
    return () => clearInterval(timer);
  }, [phase, gateway]);

  const start = useCallback(async () => {
    setError(null);
    try {
      await gateway.startDictation();
      setPhase("recording");
    } catch (raw) {
      setError(messageOf(raw));
      setPhase("idle");
    }
  }, [gateway]);

  const stop = useCallback(async () => {
    setPhase("transcribing");
    setLevel(0);
    try {
      const dictation = await gateway.stopDictation();
      onTranscript(dictation.text);
    } catch (raw) {
      // Saying nothing is an ordinary outcome, not an error worth a red box.
      const message = messageOf(raw);
      setError(message.includes("nothing was said") ? null : message);
    } finally {
      setPhase("idle");
    }
  }, [gateway, onTranscript]);

  const toggle = useCallback(() => {
    if (phaseRef.current === "recording") void stop();
    else if (phaseRef.current === "idle") void start();
    // While transcribing, do nothing: the microphone is already closed.
  }, [start, stop]);

  return { available, phase, level, error, start, stop, toggle };
}

function messageOf(raw: unknown): string {
  if (raw instanceof MemoryGatewayError) return raw.message;
  return raw instanceof Error ? raw.message : String(raw);
}
