import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import { KIND_LABELS, type Kind } from "@/lib/memory";

/**
 * The three kinds the memory domain allows, and nothing else.
 *
 * `kind` is an `INLINE_FILTER` on the vector index, so it is not decoration —
 * filtering by it happens in storage rather than by dragging candidates across
 * the network to discard them.
 */
const STYLES: Record<Kind, string> = {
  fact: "border-sky-500/25 bg-sky-500/10 text-sky-600 dark:text-sky-300",
  preference:
    "border-violet-500/25 bg-violet-500/10 text-violet-600 dark:text-violet-300",
  episode:
    "border-amber-500/25 bg-amber-500/10 text-amber-600 dark:text-amber-300",
  snippet:
    "border-emerald-500/25 bg-emerald-500/10 text-emerald-600 dark:text-emerald-300 font-mono",
};

export function KindBadge({
  kind,
  className,
}: {
  kind: Kind;
  className?: string;
}) {
  return (
    <Badge
      variant="outline"
      className={cn(
        "rounded-full px-2 py-0 text-[11px] font-medium",
        STYLES[kind],
        className,
      )}
    >
      {KIND_LABELS[kind]}
    </Badge>
  );
}
