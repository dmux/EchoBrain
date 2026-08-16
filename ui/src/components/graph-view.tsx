"use client";

/**
 * The memories as a graph, the way Obsidian draws a vault.
 *
 * # There is no stored graph, and that is the whole design problem
 *
 * A memory has `kind`, `source`, `github_repo`, a rating and its text. It has no
 * reference to another memory — no `[[wikilink]]`, no parent, no tags pointing
 * anywhere. So unlike a vault, there is nothing here to *read* and draw: every
 * edge on this screen is derived.
 *
 * The embeddings would give the honest answer directly, but they never leave the
 * backend — `repository.rs` excludes the vector from its projection on purpose,
 * with a test guarding it, because dragging a 1024-float array back on every hit
 * would inflate both the response and the billed bytes. So the client cannot
 * compute similarity between two memories on its own.
 *
 * What it *can* do is ask: `recall(memory.text)` returns that memory's nearest
 * neighbours with their distances. That is one embedding and one vector search
 * per node — real money — which is why this expands **on demand** rather than
 * pre-computing the whole graph. Clicking a node buys its neighbourhood, once,
 * and the result is cached for the session.
 *
 * The alternative, edges from shared `github_repo` or `kind`, is free and
 * instant and was rejected: it would render a filing cabinet that *looks* like a
 * meaning graph. This app's whole claim is that it searches by sense and not by
 * label, and a picture that quietly says otherwise is worse than no picture.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { Loader2, RefreshCw, Sparkles, X } from "lucide-react";

import { HitCard } from "@/components/hit-card";
import { KindBadge } from "@/components/kind-badge";
import { MarkdownContent } from "@/components/markdown-content";
import { cn } from "@/lib/utils";
import { EDGE_FLOOR, edgeKey, mergeEdges, neighboursOf } from "@/lib/graph";
import type { Edge } from "@/lib/graph";
import type { Hit, Kind, Memory, MemoryGateway } from "@/lib/memory";

/**
 * How many memories seed the canvas.
 *
 * One page of the listing, not the whole store: the layout is O(n²) per frame
 * and a thousand unconnected dots is not a graph, it is noise. Nodes past this
 * arrive by expanding into them, which is also the only way an edge exists.
 */
const SEED_LIMIT = 60;

/** Neighbours bought per expansion. Matches what the search view shows. */
const NEIGHBOURS = 8;

type Node = {
  memory: Memory;
  x: number;
  y: number;
  vx: number;
  vy: number;
  /** Bought its neighbours already. Prevents paying twice for one node. */
  expanded: boolean;
  /** Pinned by the user's cursor; the simulation leaves it alone. */
  fixed: boolean;
};

const WIDTH = 1000;
const HEIGHT = 700;

const KIND_FILL: Record<Kind, string> = {
  fact: "#0ea5e9",
  preference: "#8b5cf6",
  episode: "#f59e0b",
  snippet: "#10b981",
};

export function GraphView({ gateway }: { gateway: MemoryGateway }) {
  const [nodes, setNodes] = useState<Node[]>([]);
  const [edges, setEdges] = useState<Edge[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [expanding, setExpanding] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [hovered, setHovered] = useState<string | null>(null);

  /**
   * The live positions.
   *
   * The simulation mutates this every frame and pushes to React only on a
   * schedule below. Sixty springs at sixty hertz through `setState` would
   * re-render the whole panel — including the selected card and its markdown —
   * for a change no one can perceive between frames.
   */
  const live = useRef<Node[]>([]);
  const frame = useRef<number | null>(null);

  /**
   * Bumped by *Recomeçar* to re-run the seeding effect.
   *
   * The load is written as a promise chain inside the effect rather than as an
   * `async` callback the effect awaits, matching `LibraryView`: state is only
   * ever set from a `.then`, never synchronously in the effect body, which is
   * the cascading render React warns about.
   */
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let cancelled = false;

    gateway
      .listAll({ limit: SEED_LIMIT })
      .then((page) => {
        if (cancelled) return;
        const placed = page.memories.map((memory, index) => {
          // A ring rather than a random scatter: the simulation untangles a
          // circle reliably, while a random cloud can start with two nodes on
          // top of each other and fling them off-canvas on the first tick.
          const angle =
            (index / Math.max(page.memories.length, 1)) * Math.PI * 2;
          const radius = Math.min(WIDTH, HEIGHT) * 0.34;
          return {
            memory,
            x: WIDTH / 2 + Math.cos(angle) * radius,
            y: HEIGHT / 2 + Math.sin(angle) * radius,
            vx: 0,
            vy: 0,
            expanded: false,
            fixed: false,
          };
        });
        live.current = placed;
        setNodes(placed);
        setEdges([]);
        setSelected(null);
        setLoading(false);
      })
      .catch((raw) => {
        if (cancelled) return;
        setError(raw instanceof Error ? raw.message : String(raw));
        setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [gateway, attempt]);

  /**
   * Buy one node's neighbourhood.
   *
   * `search` with the memory's own text is the only way to ask "what is near
   * this?" from outside the backend. The memory always comes back as its own
   * best match, at similarity ~1, and is dropped — an edge from a node to
   * itself is not information.
   */
  const expand = useCallback(
    async (id: string) => {
      const node = live.current.find((entry) => entry.memory.memoryId === id);
      if (!node || node.expanded) return;

      setExpanding(id);
      try {
        const hits = await gateway.search(node.memory.text, {
          topK: NEIGHBOURS,
        });

        const known = new Set(
          live.current.map((entry) => entry.memory.memoryId),
        );
        const { added, edges: found } = neighboursOf(id, hits, known);

        node.expanded = true;
        live.current = [
          ...live.current,
          // Born next to the node that summoned them, so the springs pull them
          // into place instead of hurling them in from a corner.
          ...added.map((memory) => ({
            memory,
            x: node.x + (Math.random() - 0.5) * 80,
            y: node.y + (Math.random() - 0.5) * 80,
            vx: 0,
            vy: 0,
            expanded: false,
            fixed: false,
          })),
        ];
        setNodes([...live.current]);
        setEdges((current) => mergeEdges(current, found));
      } catch (raw) {
        setError(raw instanceof Error ? raw.message : String(raw));
      } finally {
        setExpanding(null);
      }
    },
    [gateway],
  );

  /**
   * The layout: repulsion between every pair, a spring along every edge, and a
   * weak pull to the centre so a disconnected node cannot drift off-canvas.
   *
   * O(n²) per frame, which is fine at this size and is why the seed is capped.
   */
  useEffect(() => {
    if (live.current.length === 0) return;

    let ticks = 0;
    let sinceCommit = 0;

    const step = () => {
      const current = live.current;
      const byId = new Map(current.map((node) => [node.memory.memoryId, node]));

      for (let i = 0; i < current.length; i++) {
        for (let j = i + 1; j < current.length; j++) {
          const a = current[i];
          const b = current[j];
          let dx = b.x - a.x;
          let dy = b.y - a.y;
          let distance = Math.hypot(dx, dy);
          if (distance < 0.01) {
            // Exactly coincident: nudge, or the normalised vector is NaN and
            // both nodes vanish from the canvas for the rest of the session.
            dx = Math.random() - 0.5;
            dy = Math.random() - 0.5;
            distance = 0.01;
          }
          const push = 2600 / (distance * distance);
          const ux = dx / distance;
          const uy = dy / distance;
          a.vx -= ux * push;
          a.vy -= uy * push;
          b.vx += ux * push;
          b.vy += uy * push;
        }
      }

      for (const edge of edges) {
        const a = byId.get(edge.a);
        const b = byId.get(edge.b);
        if (!a || !b) continue;
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const distance = Math.max(Math.hypot(dx, dy), 0.01);
        // Closer in meaning, closer on screen: the rest length is what makes
        // the picture readable as similarity rather than as an arbitrary web.
        const rest = 260 - edge.similarity * 160;
        const pull = (distance - rest) * 0.012;
        const ux = dx / distance;
        const uy = dy / distance;
        a.vx += ux * pull;
        a.vy += uy * pull;
        b.vx -= ux * pull;
        b.vy -= uy * pull;
      }

      for (const node of current) {
        if (node.fixed) {
          node.vx = 0;
          node.vy = 0;
          continue;
        }
        node.vx += (WIDTH / 2 - node.x) * 0.0016;
        node.vy += (HEIGHT / 2 - node.y) * 0.0016;
        node.vx *= 0.86;
        node.vy *= 0.86;
        node.x = clamp(node.x + node.vx, 24, WIDTH - 24);
        node.y = clamp(node.y + node.vy, 24, HEIGHT - 24);
      }

      ticks++;
      sinceCommit++;
      // Every third frame is imperceptible as motion and a third of the
      // renders. Below that the markdown in the side panel re-parses while the
      // graph is still settling.
      if (sinceCommit >= 3) {
        sinceCommit = 0;
        setNodes([...current]);
      }

      // Settling rather than spinning forever: a simulation left running keeps
      // a core warm on a laptop for a picture that stopped moving.
      if (ticks < 600) {
        frame.current = requestAnimationFrame(step);
      } else {
        setNodes([...current]);
        frame.current = null;
      }
    };

    frame.current = requestAnimationFrame(step);
    return () => {
      if (frame.current !== null) cancelAnimationFrame(frame.current);
      frame.current = null;
    };
  }, [edges, nodes.length]);

  const degree = new Map<string, number>();
  for (const edge of edges) {
    degree.set(edge.a, (degree.get(edge.a) ?? 0) + 1);
    degree.set(edge.b, (degree.get(edge.b) ?? 0) + 1);
  }

  const selectedNode = nodes.find((node) => node.memory.memoryId === selected);
  const neighbours: Hit[] = selected
    ? edges
        .filter((edge) => edge.a === selected || edge.b === selected)
        .map((edge) => {
          const other = edge.a === selected ? edge.b : edge.a;
          const node = nodes.find((entry) => entry.memory.memoryId === other);
          return node
            ? { memory: node.memory, similarity: edge.similarity }
            : null;
        })
        .filter((hit): hit is Hit => hit !== null)
        .sort((left, right) => right.similarity - left.similarity)
    : [];

  return (
    <div className="flex h-full flex-col gap-3">
      <div className="flex items-center justify-between gap-3">
        <p className="text-[12px] text-muted-foreground">
          {nodes.length} memórias, {edges.length} relações.{" "}
          <span className="text-muted-foreground/70">
            Clique num ponto para comprar a vizinhança dele.
          </span>
        </p>
        <button
          onClick={() => {
            setLoading(true);
            setError(null);
            setAttempt((count) => count + 1);
          }}
          className="flex items-center gap-1.5 rounded-md border border-border/60 px-2 py-1 text-[11px] font-medium text-muted-foreground transition-colors hover:border-border hover:text-foreground"
          title="Recomeçar do zero"
        >
          <RefreshCw className="size-3" />
          Recomeçar
        </button>
      </div>

      {error ? (
        <p
          role="alert"
          className="rounded-lg border border-destructive/25 bg-destructive/10 px-3 py-2 text-[13px] text-destructive"
        >
          {error}
        </p>
      ) : null}

      <div className="flex min-h-0 flex-1 gap-3">
        <div className="relative min-w-0 flex-1 overflow-hidden rounded-xl border border-border/60 bg-card/40">
          {loading ? (
            <div className="flex h-full items-center justify-center text-muted-foreground">
              <Loader2 className="size-5 animate-spin" />
            </div>
          ) : nodes.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
              <Sparkles className="size-5 text-muted-foreground/60" />
              <p className="text-[13px] text-muted-foreground">
                Nenhuma memória para desenhar ainda.
              </p>
            </div>
          ) : (
            <svg
              viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
              className="size-full"
              role="img"
              aria-label="Grafo de memórias relacionadas por significado"
            >
              {edges.map((edge) => {
                const a = nodes.find((n) => n.memory.memoryId === edge.a);
                const b = nodes.find((n) => n.memory.memoryId === edge.b);
                if (!a || !b) return null;
                const touched =
                  selected === edge.a ||
                  selected === edge.b ||
                  hovered === edge.a ||
                  hovered === edge.b;
                return (
                  <line
                    key={edgeKey(edge)}
                    x1={a.x}
                    y1={a.y}
                    x2={b.x}
                    y2={b.y}
                    stroke="currentColor"
                    // Thickness and opacity both carry the similarity, because
                    // the number itself has no room on the line.
                    strokeWidth={0.5 + (edge.similarity - EDGE_FLOOR) * 6}
                    className={cn(
                      "text-muted-foreground transition-opacity",
                      touched ? "opacity-70" : "opacity-20",
                    )}
                  />
                );
              })}

              {nodes.map((node) => {
                const id = node.memory.memoryId;
                const connections = degree.get(id) ?? 0;
                const radius = 5 + Math.min(connections, 8) * 1.4;
                const isSelected = selected === id;
                const isHovered = hovered === id;
                return (
                  <g
                    key={id}
                    onClick={() => {
                      setSelected(id);
                      void expand(id);
                    }}
                    onMouseEnter={() => setHovered(id)}
                    onMouseLeave={() => setHovered(null)}
                    className="cursor-pointer"
                  >
                    <circle
                      cx={node.x}
                      cy={node.y}
                      r={radius}
                      fill={KIND_FILL[node.memory.kind]}
                      fillOpacity={node.memory.active === false ? 0.25 : 0.85}
                      stroke="currentColor"
                      strokeWidth={isSelected ? 2 : 0}
                      className="text-foreground transition-all"
                    />
                    {expanding === id ? (
                      <circle
                        cx={node.x}
                        cy={node.y}
                        r={radius + 5}
                        fill="none"
                        stroke={KIND_FILL[node.memory.kind]}
                        strokeWidth={1.5}
                        className="animate-ping"
                      />
                    ) : null}
                    {isSelected || isHovered ? (
                      <text
                        x={node.x + radius + 4}
                        y={node.y + 4}
                        className="fill-foreground text-[11px]"
                        style={{ pointerEvents: "none" }}
                      >
                        {truncate(node.memory.text)}
                      </text>
                    ) : null}
                  </g>
                );
              })}
            </svg>
          )}

          <ul className="pointer-events-none absolute bottom-3 left-3 flex gap-3 text-[10px] text-muted-foreground">
            {(Object.keys(KIND_FILL) as Kind[]).map((kind) => (
              <li key={kind} className="flex items-center gap-1">
                <span
                  className="size-2 rounded-full"
                  style={{ background: KIND_FILL[kind] }}
                />
                {kind}
              </li>
            ))}
          </ul>
        </div>

        {selectedNode ? (
          <aside className="flex w-[360px] shrink-0 flex-col gap-3 overflow-y-auto rounded-xl border border-border/60 bg-card/40 p-3">
            <div className="flex items-start justify-between gap-2">
              <KindBadge kind={selectedNode.memory.kind} />
              <button
                onClick={() => setSelected(null)}
                className="rounded-md p-1 text-muted-foreground hover:bg-muted/50 hover:text-foreground"
                aria-label="Fechar"
              >
                <X className="size-3.5" />
              </button>
            </div>

            {/*
              The selected memory gets no similarity meter, deliberately: it is
              not a hit, it is the thing everything else is being measured
              against. A meter here would have to invent a number.
            */}
            <MarkdownContent content={selectedNode.memory.text} />

            <p className="text-[11px] text-muted-foreground/70">
              {new Date(selectedNode.memory.createdAt * 1000).toLocaleDateString(
                "pt-BR",
              )}
              {selectedNode.memory.source ? ` · ${selectedNode.memory.source}` : ""}
            </p>

            <div className="border-t border-border/50 pt-3">
              <h3 className="mb-2 text-[11px] font-medium text-muted-foreground">
                {expanding === selected
                  ? "Buscando relações…"
                  : neighbours.length > 0
                    ? `${neighbours.length} relacionadas`
                    : selectedNode.expanded
                      ? "Nada acima do piso de similaridade."
                      : "Clique de novo para buscar relações."}
              </h3>
              <div className="space-y-2">
                {neighbours.map((hit, index) => (
                  <HitCard
                    key={hit.memory.memoryId}
                    hit={hit}
                    index={index}
                    gateway={gateway}
                  />
                ))}
              </div>
            </div>
          </aside>
        ) : null}
      </div>
    </div>
  );
}

function truncate(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > 42 ? `${flat.slice(0, 42)}…` : flat;
}

function clamp(value: number, low: number, high: number): number {
  return Math.min(Math.max(value, low), high);
}
