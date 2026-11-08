export interface Match {
  score: number;
  positions: number[];
}

const SEPARATORS = new Set(["/", "_", "-", ".", " "]);

/** Keep each character's UTF-16 length unchanged so match indices still align with `text`. */
function lower(text: string): string {
  return text.replace(/[\s\S]/gu, (char) => {
    const lowered = char.toLowerCase();
    return lowered.length === char.length ? lowered : char;
  });
}

export function fuzzyMatch(query: string, text: string): Match | null {
  if (query === "") return { score: 0, positions: [] };
  const q = lower(query);
  const t = lower(text);
  const positions: number[] = [];
  let score = 0;
  let from = 0;
  // Iterate by Unicode code point so surrogate pairs count as a single match.
  let queryIndex = 0;
  for (const char of q) {
    const at = t.indexOf(char, from);
    if (at < 0) return null;
    const previous = positions[positions.length - 1];
    if (previous !== undefined && at === previous + 1) score += 5;
    if (at === 0 || SEPARATORS.has(t[at - 1] ?? "")) score += 8;
    if (text.startsWith(query.slice(queryIndex, queryIndex + char.length), at)) score += 1;
    score -= Math.min(at - from, 10) * 0.5;
    for (let unit = 0; unit < char.length; unit++) positions.push(at + unit);
    from = at + char.length;
    queryIndex += char.length;
  }
  score -= text.length * 0.05;
  if ((positions[0] ?? 0) > text.lastIndexOf("/")) score += 6;
  return { score, positions };
}

export function fuzzyFilter<T>(items: readonly T[], query: string, text: (item: T) => string): { item: T; match: Match }[] {
  const results: { item: T; match: Match; index: number }[] = [];
  items.forEach((item, index) => {
    const match = fuzzyMatch(query, text(item));
    if (match) results.push({ item, match, index });
  });
  results.sort((a, b) => b.match.score - a.match.score || a.index - b.index);
  return results.map(({ item, match }) => ({ item, match }));
}
