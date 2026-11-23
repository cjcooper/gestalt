// Go to File: ranking the opened folder's paths against what's typed, and
// completing a typed path a folder at a time (Tab), like a shell.

/** A path below the opened folder, as `find_files` lists it (search.rs). */
export interface PathItem {
  /** `/`-separated, relative to the opened folder. */
  rel: string;
  /** Absolute, as the tree spells it. */
  path: string;
  isDir: boolean;
}

export interface PathMatch {
  item: PathItem;
  /** The path as shown: a folder's ends in `/`. */
  text: string;
  /** Indices in `text` that matched, to emphasise. */
  hits: number[];
}

/** What's typed, as a path: `\` as `/`, no leading `./` or `/`, no surrounding space. */
export function normalizeQuery(query: string): string {
  return query.trim().replace(/\\/g, "/").replace(/^(\.\/|\/)+/, "");
}

const range = (from: number, length: number) => Array.from({ length }, (_, i) => from + i);

/** Positions of `q`'s characters in order in `t` (both lower case), or null. */
function subsequence(t: string, q: string): number[] | null {
  const hits: number[] = [];
  let at = 0;
  for (const ch of q) {
    at = t.indexOf(ch, at);
    if (at < 0) return null;
    hits.push(at++);
  }
  return hits;
}

/**
 * The best matches for `query`, best first:
 *  0. the path starts with it (what you get typing a path); nearest first,
 *     so a folder's own contents come before what's deeper in it,
 *  1. the name is it, 2. the name starts with it, 3. the name contains it,
 *  4. the path contains it,
 *  5. its characters appear in order in the path (`eprm` for envs/prod/main.tf).
 * Ties go to the shorter path, then folders, then by name.
 */
export function matchPaths(items: PathItem[], query: string, limit = 100): PathMatch[] {
  const q = normalizeQuery(query).toLowerCase();
  if (!q) return [];
  // Spaces only separate parts: `prod main` finds envs/prod/main.tf.
  const fuzzy = q.replace(/\s+/g, "");
  const scored: { m: PathMatch; tier: number; rank: number }[] = [];
  for (const item of items) {
    const text = item.isDir ? `${item.rel}/` : item.rel;
    const t = text.toLowerCase();
    const nameAt = t.lastIndexOf("/", t.length - 2) + 1;
    const name = t.slice(nameAt, item.isDir ? -1 : undefined);
    const nameHit = name.indexOf(q);
    const pathHit = t.indexOf(q);
    let tier: number;
    let hits: number[];
    let rank = text.length;
    if (pathHit === 0) {
      tier = 0;
      hits = range(0, q.length);
      // Folders below the typed part, not counting a folder's own trailing `/`.
      rank = t.slice(q.length, item.isDir ? -1 : undefined).split("/").length;
    } else if (nameHit >= 0) {
      tier = name === q ? 1 : nameHit === 0 ? 2 : 3;
      hits = range(nameAt + nameHit, q.length);
    } else if (pathHit > 0) {
      tier = 4;
      hits = range(pathHit, q.length);
    } else {
      const sub = subsequence(t, fuzzy);
      if (!sub) continue;
      tier = 5;
      // Tighter is better.
      rank = sub[sub.length - 1] - sub[0];
      hits = sub;
    }
    scored.push({ m: { item, text, hits }, tier, rank });
  }
  scored.sort(
    (a, b) =>
      a.tier - b.tier ||
      a.rank - b.rank ||
      Number(b.m.item.isDir) - Number(a.m.item.isDir) ||
      a.m.text.toLowerCase().localeCompare(b.m.text.toLowerCase()),
  );
  return scored.slice(0, limit).map((s) => s.m);
}

/**
 * Tab: complete `query` towards `text` (the chosen match) up to the end of
 * the next folder, as a shell does; a match that isn't a continuation of
 * what's typed is taken whole.
 */
export function completePath(query: string, text: string): string {
  const q = normalizeQuery(query);
  if (!text.toLowerCase().startsWith(q.toLowerCase())) return text;
  const next = text.indexOf("/", q.length);
  return next < 0 ? text : text.slice(0, next + 1);
}
