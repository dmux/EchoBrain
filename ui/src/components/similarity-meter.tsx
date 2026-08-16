import { cn } from "@/lib/utils";

/**
 * How close a hit is, drawn as a filled track.
 *
 * The value is *similarity*, never distance. A COSINE index scores 0 for
 * identical and 2 for opposite, so a bar filled in proportion to distance would
 * draw the best match as the emptiest one. `echobrain-core` inverts it once, in
 * `Similarity::from_distance`, and nothing downstream ever sees the raw metric.
 */
export function SimilarityMeter({
  value,
  className,
}: {
  /** 0..=1, where 1 is identical. */
  value: number;
  className?: string;
}) {
  const percent = Math.round(Math.min(1, Math.max(0, value)) * 100);

  return (
    <div className={cn("flex items-center gap-2", className)}>
      <div
        className="relative h-1 w-16 overflow-hidden rounded-full bg-foreground/10"
        role="meter"
        aria-valuenow={percent}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label="Similaridade"
      >
        <div
          className="absolute inset-y-0 left-0 rounded-full bg-gradient-to-r from-sky-500 to-violet-500 transition-[width] duration-500 ease-out"
          style={{ width: `${percent}%` }}
        />
      </div>
      <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
        {percent}%
      </span>
    </div>
  );
}
