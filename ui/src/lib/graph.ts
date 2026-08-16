/**
 * The rules that turn a recall into graph edges.
 *
 * Pure, and separate from `GraphView`, because these three decisions are the
 * only place the picture can start lying — everything else in that component is
 * springs and circles. Kept here so they can be exercised without a renderer.
 */

import type { Hit, Memory } from "@/lib/memory";

/**
 * Below this a neighbour is not drawn.
 *
 * A COSINE index always returns something. Ask for the neighbours of anything
 * and the weakest of the eight will be an unrelated note that merely shares a
 * language — against a populated namespace, an unrelated memory still scores
 * around 0.62 (measured; see `usecase/answer.rs`). Drawing that puts a line
 * between two memories that have nothing to do with each other, and a line is
 * read as a claim.
 *
 * Lower than the answering floor of 0.65 on purpose: a faint edge the user can
 * see and dismiss is a smaller claim than a sentence composed from the same hit
 * and read aloud. Same reasoning, different stakes.
 */
export const EDGE_FLOOR = 0.6;

/** An undirected edge, weighted by how close the two memories are in meaning. */
export type Edge = { a: string; b: string; similarity: number };

/**
 * The key two endpoints share regardless of order.
 *
 * Expanding A finds B, and later expanding B finds A. Without this the same
 * relationship is drawn twice, at two thicknesses, and the count under the
 * canvas reads double.
 */
export function edgeKey(edge: Edge): string {
  return edge.a < edge.b ? `${edge.a}|${edge.b}` : `${edge.b}|${edge.a}`;
}

/**
 * What one expansion adds to the graph.
 *
 * `known` is the set of memory ids already on the canvas; ids in it produce an
 * edge but no duplicate node.
 */
export function neighboursOf(
  sourceId: string,
  hits: Hit[],
  known: ReadonlySet<string>,
): { added: Memory[]; edges: Edge[] } {
  const added: Memory[] = [];
  const edges: Edge[] = [];
  const seen = new Set(known);

  for (const hit of hits) {
    const other = hit.memory.memoryId;

    // A memory is always its own best match, at similarity ~1. An edge from a
    // node to itself is not information, and the loop it draws is a distraction
    // on every single node.
    if (other === sourceId) continue;
    if (hit.similarity < EDGE_FLOOR) continue;

    if (!seen.has(other)) {
      seen.add(other);
      added.push(hit.memory);
    }
    edges.push({ a: sourceId, b: other, similarity: hit.similarity });
  }

  return { added, edges };
}

/** Merge new edges into existing ones, keeping each relationship once. */
export function mergeEdges(current: Edge[], incoming: Edge[]): Edge[] {
  const seen = new Set(current.map(edgeKey));
  const merged = [...current];
  for (const edge of incoming) {
    const key = edgeKey(edge);
    if (seen.has(key)) continue;
    seen.add(key);
    merged.push(edge);
  }
  return merged;
}
