// Fuzzy matching for the quick pickers: the query's characters must appear
// in order. Matches at the start, after a separator, or in a run score higher.

export interface Match {
  score: number;
  /** Indices of the matched characters, for highlighting. */
  positions: number[];
}

const SEPARATORS = new Set(["/", "_", "-", ".", " "]);

export function fuzzyMatch(query: string, text: string): Match | null {
  if (query === "") return { score: 0, positions: [] };
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  const positions: number[] = [];
  let score = 0;
  let from = 0;
  for (const char of q) {
    const at = t.indexOf(char, from);
    if (at < 0) return null;
    const previous = positions[positions.length - 1];
    if (previous !== undefined && at === previous + 1) score += 5;
    if (at === 0 || SEPARATORS.has(t[at - 1] ?? "")) score += 8;
    if (text[at] === query[positions.length]) score += 1;
    score -= Math.min(at - from, 10) * 0.5;
    positions.push(at);
    from = at + 1;
  }
  // Prefer shorter texts, and matches in the last path segment (the name).
  score -= text.length * 0.05;
  if ((positions[0] ?? 0) > text.lastIndexOf("/")) score += 6;
  return { score, positions };
}

/** The items matching `query`, best first (ties keep their order). */
export function fuzzyFilter<T>(items: readonly T[], query: string, text: (item: T) => string): { item: T; match: Match }[] {
  const results: { item: T; match: Match; index: number }[] = [];
  items.forEach((item, index) => {
    const match = fuzzyMatch(query, text(item));
    if (match) results.push({ item, match, index });
  });
  results.sort((a, b) => b.match.score - a.match.score || a.index - b.index);
  return results.map(({ item, match }) => ({ item, match }));
}
