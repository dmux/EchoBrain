import { useCallback, useEffect, useRef, useState } from "react";
import {
  type Memory,
  type MemoryFilter,
  type MemoryGateway,
  KINDS,
  KIND_LABELS,
  Kind,
} from "@/lib/memory";
import { KindBadge } from "@/components/kind-badge";
import { MarkdownContent } from "@/components/markdown-content";
import {
  Loader2,
  Library as LibraryIcon,
  Trash2,
  Edit2,
  Star,
  X,
  Save,
  Search,
  FolderGit2,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

const formatter = new Intl.RelativeTimeFormat("pt-BR", { numeric: "auto" });

function formatRelative(secondsSinceEpoch: number): string {
  const diffInSeconds = secondsSinceEpoch - Math.floor(Date.now() / 1000);
  const abs = Math.abs(diffInSeconds);
  if (abs < 60) return "agora mesmo";
  if (abs < 3600) return formatter.format(Math.round(diffInSeconds / 60), "minute");
  if (abs < 86400) return formatter.format(Math.round(diffInSeconds / 3600), "hour");
  return formatter.format(Math.round(diffInSeconds / 86400), "day");
}

function RatingStars({ rating, onChange }: { rating: number; onChange?: (r: number) => void }) {
  return (
    <div className="flex items-center gap-1">
      {[1, 2, 3, 4, 5].map((star) => (
        <button
          key={star}
          type="button"
          onClick={() => onChange?.(rating === star ? 0 : star)}
          disabled={!onChange}
          className={cn(
            "p-0.5 transition-colors",
            !onChange && "cursor-default",
            star <= rating ? "text-amber-400" : "text-muted-foreground/30",
            onChange && "hover:text-amber-400/80"
          )}
        >
          <Star className="size-4 fill-current" />
        </button>
      ))}
    </div>
  );
}

function MemoryRow({ 
  memory, 
  gateway, 
  onDeleted 
}: { 
  memory: Memory; 
  gateway: MemoryGateway; 
  onDeleted: () => void 
}) {
  const [isEditing, setIsEditing] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [text, setText] = useState(memory.text);
  const [kind, setKind] = useState<Kind>(memory.kind);
  const [rating, setRating] = useState<number>(memory.rating || 0);
  const [active, setActive] = useState<boolean>(memory.active ?? true);
  /**
   * Whether the trash button is armed.
   *
   * This replaces `window.confirm`, which is not a portable way to ask: the app
   * ships no dialog plugin, so the Tauri webview has nothing to render the
   * native panel with and `confirm()` returns `false` immediately. Deleting
   * from the library did nothing at all, silently — and worked in `make ui`,
   * where a real browser answers the prompt, which is why it survived review.
   */
  const [armed, setArmed] = useState(false);
  /** Failures used to reach `console.error` and nowhere the user could look. */
  const [failure, setFailure] = useState<string | null>(null);

  const handleSave = async () => {
    setIsSaving(true);
    setFailure(null);
    try {
      await gateway.update(memory.memoryId, {
        text,
        kind,
        rating: rating > 0 ? rating : null,
        active
      });
      setIsEditing(false);
    } catch (e) {
      setFailure(String(e));
    } finally {
      setIsSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!armed) {
      setArmed(true);
      return;
    }
    setFailure(null);
    try {
      await gateway.forget(memory.memoryId);
      onDeleted();
    } catch (e) {
      setArmed(false);
      setFailure(String(e));
    }
  };

  const handleToggleActive = async (checked: boolean) => {
    setActive(checked);
    setFailure(null);
    try {
      await gateway.update(memory.memoryId, { active: checked });
    } catch (e) {
      setActive(!checked); // revert on error
      setFailure(String(e));
    }
  };

  const handleRatingChange = async (newRating: number) => {
    setRating(newRating);
    setFailure(null);
    try {
      await gateway.update(memory.memoryId, { rating: newRating > 0 ? newRating : null });
    } catch (e) {
      setRating(memory.rating || 0); // revert
      setFailure(String(e));
    }
  };

  return (
    <div className={cn("group flex flex-col gap-3 rounded-xl border p-4 transition-all duration-300", !active && "opacity-60 bg-muted/30")}>
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center gap-3 flex-wrap">
          {isEditing ? (
            <Select value={kind} onValueChange={(v) => setKind(v as Kind)}>
              <SelectTrigger className="h-7 w-[130px] text-xs">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {KINDS.map((k) => (
                  <SelectItem key={k} value={k} className="text-xs">{KIND_LABELS[k]}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          ) : (
            <KindBadge kind={kind} />
          )}
          
          <div className="flex items-center gap-2">
            <span className="text-[11px] text-muted-foreground uppercase tracking-wider font-semibold">Avaliação</span>
            <RatingStars rating={rating} onChange={isEditing ? setRating : handleRatingChange} />
          </div>

          <div className="flex items-center gap-2">
            <span className="text-[11px] text-muted-foreground uppercase tracking-wider font-semibold">Ativa</span>
            <Switch checked={active} onCheckedChange={isEditing ? setActive : handleToggleActive} className="scale-75 origin-left" />
          </div>
          
          {(memory.githubLogin || memory.githubRepo) && (
            <div className="flex items-center gap-1.5 px-2 py-0.5 rounded-full bg-secondary/50 text-[11px] text-secondary-foreground">
              <span className="opacity-70">GitHub:</span>
              <span className="font-medium">{memory.githubRepo || memory.githubLogin}</span>
            </div>
          )}
        </div>
        
        <div className="flex items-center gap-2 opacity-40 group-hover:opacity-100 transition-opacity">
          {isEditing ? (
            <>
              <Button size="icon" variant="ghost" className="size-7" onClick={() => {
                setIsEditing(false);
                setText(memory.text);
                setKind(memory.kind);
                setRating(memory.rating || 0);
                setActive(memory.active ?? true);
              }} disabled={isSaving}>
                <X className="size-3.5" />
              </Button>
              <Button size="icon" variant="default" className="size-7" onClick={handleSave} disabled={isSaving}>
                {isSaving ? <Loader2 className="size-3.5 animate-spin" /> : <Save className="size-3.5" />}
              </Button>
            </>
          ) : (
            <>
              <Button size="icon" variant="ghost" className="size-7" onClick={() => setIsEditing(true)}>
                <Edit2 className="size-3.5" />
              </Button>
              {armed ? (
                // Armed state carries the whole warning: the button says what
                // the next click does, and clicking anywhere else disarms it.
                <>
                  <Button
                    size="sm"
                    variant="destructive"
                    className="h-7 px-2 text-[11px]"
                    onClick={handleDelete}
                  >
                    Apagar mesmo?
                  </Button>
                  <Button
                    size="icon"
                    variant="ghost"
                    className="size-7"
                    onClick={() => setArmed(false)}
                    title="Cancelar"
                  >
                    <X className="size-3.5" />
                  </Button>
                </>
              ) : (
                <Button size="icon" variant="ghost" className="size-7 text-destructive hover:bg-destructive/10 hover:text-destructive" onClick={handleDelete} title="Apagar memória">
                  <Trash2 className="size-3.5" />
                </Button>
              )}
            </>
          )}
        </div>
      </div>

      <div className="mt-1">
        {isEditing ? (
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            className="w-full min-h-[80px] text-[13.5px] p-3 rounded-md border bg-background resize-y focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          />
        ) : (
          <div className="text-[13.5px]">
            <MarkdownContent content={text} />
          </div>
        )}
      </div>

      {failure ? (
        <p className="rounded-lg border border-destructive/25 bg-destructive/10 px-3 py-2 text-[12px] text-destructive">
          {failure}
        </p>
      ) : null}

      <div className="flex items-center justify-between mt-1 text-[11px] text-muted-foreground">
        <span>{formatRelative(memory.createdAt)}</span>
        <span className="font-mono opacity-50">{memory.memoryId.slice(0,8)}...</span>
      </div>
    </div>
  );
}

/** Rows per request. Small enough that the first screen arrives quickly. */
const PAGE_SIZE = 25;

/** `undefined` for "any" — the shape {@link MemoryFilter} uses for "no filter". */
const ANY = "__any__";

/**
 * The filter controls.
 *
 * Every change restarts the listing from page one, which is why they are
 * plain values reported upward rather than local state: a filter that lagged
 * behind the rows on screen would be worse than no filter.
 */
function FilterBar({
  filter,
  onChange,
}: {
  filter: MemoryFilter;
  onChange: (next: MemoryFilter) => void;
}) {
  /**
   * The text and repository boxes are debounced: they restart the listing on
   * every change, and doing that per keystroke would fire a request per letter
   * — each one a billed read against DynamoDB.
   */
  const [draft, setDraft] = useState({
    textContains: filter.textContains ?? "",
    githubRepo: filter.githubRepo ?? "",
  });

  useEffect(() => {
    const timer = setTimeout(() => {
      const text = draft.textContains.trim();
      const repo = draft.githubRepo.trim();
      if ((filter.textContains ?? "") === text && (filter.githubRepo ?? "") === repo) {
        return;
      }
      onChange({
        ...filter,
        textContains: text || undefined,
        githubRepo: repo || undefined,
      });
    }, 350);
    return () => clearTimeout(timer);
  }, [draft, filter, onChange]);

  const set = (patch: Partial<MemoryFilter>) => onChange({ ...filter, ...patch });
  const active = Object.values(filter).some((value) => value !== undefined);

  return (
    <div className="flex flex-wrap items-center gap-2 rounded-xl border border-border/60 bg-muted/20 p-2">
      <div className="relative min-w-[180px] flex-1">
        <Search className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={draft.textContains}
          onChange={(e) => setDraft((d) => ({ ...d, textContains: e.target.value }))}
          placeholder="Contém o texto…"
          className="h-8 pl-7 text-xs"
        />
      </div>

      <div className="relative min-w-[160px]">
        <FolderGit2 className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={draft.githubRepo}
          onChange={(e) => setDraft((d) => ({ ...d, githubRepo: e.target.value }))}
          placeholder="dono/repositório"
          className="h-8 pl-7 text-xs"
        />
      </div>

      <Select
        value={filter.kind ?? ANY}
        onValueChange={(v) => set({ kind: v === ANY ? undefined : (v as Kind) })}
      >
        <SelectTrigger className="h-8 w-[120px] text-xs">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ANY} className="text-xs">Todo tipo</SelectItem>
          {KINDS.map((k) => (
            <SelectItem key={k} value={k} className="text-xs">{KIND_LABELS[k]}</SelectItem>
          ))}
        </SelectContent>
      </Select>

      <Select
        value={filter.active === undefined ? ANY : String(filter.active)}
        onValueChange={(v) => set({ active: v === ANY ? undefined : v === "true" })}
      >
        <SelectTrigger className="h-8 w-[120px] text-xs">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ANY} className="text-xs">Ativas e não</SelectItem>
          <SelectItem value="true" className="text-xs">Só ativas</SelectItem>
          <SelectItem value="false" className="text-xs">Só desativadas</SelectItem>
        </SelectContent>
      </Select>

      <Select
        value={filter.minRating === undefined ? ANY : String(filter.minRating)}
        onValueChange={(v) => set({ minRating: v === ANY ? undefined : Number(v) })}
      >
        <SelectTrigger className="h-8 w-[130px] text-xs">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ANY} className="text-xs">Qualquer nota</SelectItem>
          {[1, 2, 3, 4, 5].map((stars) => (
            <SelectItem key={stars} value={String(stars)} className="text-xs">
              {stars}+ estrelas
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      {active ? (
        <Button
          size="sm"
          variant="ghost"
          className="h-8 px-2 text-[11px]"
          onClick={() => {
            setDraft({ textContains: "", githubRepo: "" });
            onChange({});
          }}
        >
          <X className="mr-1 size-3" />
          Limpar
        </Button>
      ) : null}
    </div>
  );
}


export function LibraryView({ gateway }: { gateway: MemoryGateway }) {
  const [filter, setFilter] = useState<MemoryFilter>({});
  /**
   * Bumped to start the listing over: a filter changed, or the refresh button
   * was pressed.
   *
   * It is the list's `key`, so a bump **remounts** it. That is the whole reset
   * mechanism — no effect clears the rows, no in-flight request has to be
   * matched against the filter that asked for it, and a response that arrives
   * after a filter change lands in a component that no longer exists.
   */
  const [generation, setGeneration] = useState(0);
  const restart = useCallback(() => setGeneration((n) => n + 1), []);

  const changeFilter = useCallback((next: MemoryFilter) => {
    setFilter(next);
    setGeneration((n) => n + 1);
  }, []);

  return (
    <div className="flex w-full flex-col space-y-4">
      <div className="flex items-center justify-between pb-2 border-b">
        <div>
          <h2 className="text-lg font-semibold tracking-tight">Biblioteca de Memórias</h2>
          <p className="text-sm text-muted-foreground">
            Tudo que existe nesta conta — inclusive o que o MCP e os agentes escreveram.
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={restart}>
          Atualizar
        </Button>
      </div>

      <FilterBar filter={filter} onChange={changeFilter} />

      <MemoryList
        key={generation}
        gateway={gateway}
        filter={filter}
        onDeleted={restart}
      />
    </div>
  );
}

/**
 * The paged rows. Remounted whenever the listing has to start over.
 *
 * Owning the pages here rather than in {@link LibraryView} is what keeps the
 * reset honest: the state cannot outlive the filter that produced it, because
 * it is destroyed with the component.
 */
function MemoryList({
  gateway,
  filter,
  onDeleted,
}: {
  gateway: MemoryGateway;
  filter: MemoryFilter;
  onDeleted: () => void;
}) {
  const [memories, setMemories] = useState<Memory[]>([]);
  /** `undefined` while the first page is in flight; `null` once the list ends. */
  const [cursor, setCursor] = useState<string | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);

  /**
   * Which page to fetch. Raised by the scroll sentinel, and the only input the
   * fetching effect has — so loading a page is a state change, not a call, and
   * every `setState` lands in a promise callback rather than in an effect body.
   */
  const [wanted, setWanted] = useState<{ cursor?: string }>({});

  useEffect(() => {
    let cancelled = false;

    gateway
      .listAll({ limit: PAGE_SIZE, cursor: wanted.cursor, filter })
      .then(
        (page) => {
          if (cancelled) return;
          setMemories((current) =>
            wanted.cursor === undefined ? page.memories : [...current, ...page.memories],
          );
          setCursor(page.nextCursor);
          setError(null);
        },
        (e) => {
          if (cancelled) return;
          setError(String(e));
          // Stop the scroll rather than retrying forever against a backend that
          // is failing.
          setCursor(null);
        },
      );

    return () => {
      cancelled = true;
    };
    // `filter` is fixed for this component's lifetime — a change remounts it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gateway, wanted]);

  /**
   * The infinite scroll.
   *
   * An `IntersectionObserver` on a sentinel below the last row, rather than a
   * scroll listener: the panel scrolls inside a container whose height changes
   * with the window, and the observer needs to know none of that.
   *
   * It re-arms while the sentinel is still visible, which is what carries the
   * scroll past a filtered page that came back empty — the case where a "load
   * more" button would leave someone clicking through nothing.
   */
  const sentinel = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const node = sentinel.current;
    // `null` is the end. `undefined` means the first page is still on its way,
    // and asking for the next one would race it.
    if (!node || cursor == null) return;

    const observer = new IntersectionObserver(
      (entries) => {
        if (entries[0]?.isIntersecting) setWanted({ cursor });
      },
      // Fetch before the sentinel is actually on screen, so the next rows are
      // usually there by the time the user scrolls to them.
      { rootMargin: "200px" },
    );

    observer.observe(node);
    return () => observer.disconnect();
  }, [cursor]);

  const loading = cursor === undefined || (cursor !== null && wanted.cursor === cursor);
  const atEnd = cursor === null;
  const filtered = Object.values(filter).some((value) => value !== undefined);

  return (
    <>
      {error ? (
        <p className="rounded-lg border border-destructive/25 bg-destructive/10 px-3 py-2 text-[13px] text-destructive">
          {error}
        </p>
      ) : null}

      {memories.length === 0 && !atEnd ? (
        <div className="flex h-32 items-center justify-center">
          <Loader2 className="size-5 animate-spin text-muted-foreground" />
        </div>
      ) : memories.length === 0 ? (
        <div className="flex h-40 flex-col items-center justify-center rounded-xl border border-dashed border-border/50 text-center">
          <LibraryIcon className="mb-2 size-6 text-muted-foreground/50" />
          <p className="text-[13.5px] font-medium">
            {filtered ? "Nenhuma memória com esses filtros." : "Sua biblioteca está vazia."}
          </p>
          <p className="mt-1 max-w-sm text-[12.5px] text-muted-foreground">
            {filtered
              ? "Tente afrouxar os filtros — a busca por texto aqui é literal, não semântica."
              : "As memórias que você guardar aparecerão aqui."}
          </p>
        </div>
      ) : (
        <div className="space-y-3">
          {memories.map((memory) => (
            <MemoryRow
              key={memory.memoryId}
              memory={memory}
              gateway={gateway}
              onDeleted={onDeleted}
            />
          ))}
        </div>
      )}

      {/*
        Kept mounted while there is more to load, including mid-request:
        unmounting it disconnects the observer, and the scroll would then need a
        nudge to pick up again.
      */}
      <div ref={sentinel} className="h-px" />

      {memories.length > 0 ? (
        <p className="pb-2 text-center text-[11px] text-muted-foreground">
          {atEnd ? (
            `${memories.length} ${memories.length === 1 ? "memória" : "memórias"}`
          ) : (
            <span className="inline-flex items-center gap-1.5">
              {loading ? <Loader2 className="size-3 animate-spin" /> : null}
              carregando mais…
            </span>
          )}
        </p>
      ) : null}
    </>
  );
}
