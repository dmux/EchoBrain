"use client";

import { useEffect, useRef, useState } from "react";

import { cn } from "@/lib/utils";

/** How often the meter scrolls. Slower than the poll, so it reads as motion. */
const SHIFT_MS = 70;

/**
 * A rolling bar meter of the microphone input.
 *
 * It exists to answer one question at a glance: *is it hearing me?* macOS hands
 * an app that lacks microphone permission **silence** rather than an error, so
 * without a visible level a dead microphone looks identical to a working one
 * until the transcript comes back wrong — or worse, comes back as a confident
 * hallucination over the silence.
 *
 * Mounted only while a dictation is in progress, so there is no stale history
 * to clear: the component starts empty because it starts new.
 */
export function Waveform({
  level,
  active,
  bars = 28,
  className,
}: {
  /** 0..=1. */
  level: number;
  active: boolean;
  bars?: number;
  className?: string;
}) {
  const [history, setHistory] = useState<number[]>(() =>
    new Array(bars).fill(0),
  );

  // The interval reads the newest level without being re-created on every
  // change, which at ~30 fps would mean tearing down a timer 30 times a second.
  const latest = useRef(level);
  useEffect(() => {
    latest.current = level;
  }, [level]);

  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => {
      setHistory((previous) => [...previous.slice(1), latest.current]);
    }, SHIFT_MS);
    return () => clearInterval(timer);
  }, [active]);

  return (
    <div
      className={cn("flex h-12 items-center justify-center gap-1", className)}
      aria-hidden
    >
      {history.map((value, index) => {
        // Smooth out the edges using a bell curve multiplier
        const center = (bars - 1) / 2;
        const distance = Math.abs(index - center) / (bars / 2);
        const multiplier = Math.max(0.1, 1 - Math.pow(distance, 2));
        const activeValue = Math.min(1, value) * multiplier;

        return (
          <span
            key={index}
            className="w-1.5 rounded-full bg-gradient-to-t from-pink-500 via-purple-500 to-sky-500 transition-all duration-75 ease-out"
            style={{
              height: `${Math.max(4, activeValue * 48)}px`,
              opacity: active ? 0.4 + activeValue * 0.6 : 0.15,
              boxShadow: active ? `0 0 ${8 + activeValue * 12}px rgba(168, 85, 247, ${0.3 + activeValue * 0.4})` : 'none',
            }}
          />
        );
      })}
    </div>
  );
}
