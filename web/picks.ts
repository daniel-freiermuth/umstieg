// Narrowing down by picks, without the DOM: clicked lines and stations, what
// connections must pass, and what is reachable through each.

import type { Need } from "./best";
import type { Pt, XY } from "./geometry";
import { type Reach, type Spot, intersectReach } from "./reach";
import { item } from "./util";

export interface Picked {
  id: string;
  info: string;
  /** Where it is drawn: a stretch, a stay, or a station's row. */
  pts: readonly XY[];
  /** Its points as (time, station), whatever the metric: where it is drawn at any metric's heights. */
  raw: readonly (readonly [t: number, s: number])[];
  /** What a connection passing it must do there. */
  need: Need | null;
  reach: Reach | null;
  /** Where it lies: a stretch of a piece, or a moment at a station; none for a station's row. */
  spot?: Spot;
}
/** Together, the picks: their names, what connections must pass, the shared highlight. */
export interface Narrowed {
  info: string;
  needs: Need[];
  reach: Reach | null;
}

/**
 * What connections must pass to pass every pick, and the highlight
 * reachable through every pick that has one: per piece, the stretch all their
 * highlights share. Nothing without picks.
 */
export function narrow(picks: readonly Picked[]): Narrowed | null {
  if (picks.length === 0) return null;
  const reaches = picks.map((p) => p.reach).filter((r): r is Reach => r !== null);
  const reach = reaches.length
    ? {
        key: picks.map((p) => p.reach?.key ?? p.id).join("|"),
        segments: intersectReach(reaches.map((r) => r.segments)),
      }
    : null;
  const needs = picks.flatMap((p) => (p.need === null ? [] : [p.need]));
  return { info: picks.map((p) => p.info).join(" + "), needs, reach };
}

/** The selection after the candidates change: the only one, else the old one if still listed. */
export function pinAfter(chains: readonly number[], pinned: number | null): number | null {
  if (chains.length === 1) return item(chains, 0);
  return pinned !== null && chains.includes(pinned) ? pinned : null;
}

/**
 * The selection after stepping through the list (↓ or ↑), wrapping around;
 * the first one when nothing listed is selected, none in an empty list.
 */
export function step(list: readonly number[], pinned: number | null, down: boolean): number | null {
  const now = pinned === null ? -1 : list.indexOf(pinned);
  const next = now < 0 ? 0 : (now + (down ? 1 : list.length - 1)) % list.length;
  return list[next] ?? null;
}

/** Picks with the target toggled: removed if picked (by id), else appended. */
export function toggle(picks: readonly Picked[], target: Picked): Picked[] {
  return picks.some((p) => p.id === target.id) ? picks.filter((p) => p.id !== target.id) : [...picks, target];
}

/**
 * The picks drawn at another metric's heights: a stretch of a piece as that
 * piece's line now runs there; a stay or a station's row from its points as
 * picked, at its station's height in `heights`, left out without one (as on
 * its line).
 */
export function atHeights(
  picks: readonly Picked[],
  pieces: readonly { raw: readonly (readonly [t: number, s: number])[] }[],
  heights: readonly (number | null)[],
): Picked[] {
  return picks.map((p) => {
    const spot = p.spot;
    if (spot !== undefined && "piece" in spot) {
      // Its first and last call, found among the piece's by time and station.
      const { raw } = item(pieces, spot.piece);
      const [first, last] = [p.raw[0], p.raw[p.raw.length - 1]];
      const from = raw.findIndex(([t, s]) => t === first?.[0] && s === first[1]);
      const to = from < 0 ? -1 : raw.findIndex(([t, s], k) => k > from && t === last?.[0] && s === last[1]);
      return { ...p, pts: to < 0 ? [] : stretch(raw, heights, from, to) };
    }
    return {
      ...p,
      pts: p.raw.flatMap(([t, s]): XY[] => {
        const h = heights[s] ?? null;
        return h === null ? [] : [[t, h, s]];
      }),
    };
  });
}

/**
 * A piece's line at `heights` from its call `from` to its call `to`
 * (positions in `raw`), cut at their times where they have no height; the
 * hops before and after are left out, even those taking no time.
 */
function stretch(
  raw: readonly (readonly [t: number, s: number])[],
  heights: readonly (number | null)[],
  from: number,
  to: number,
): Pt[] {
  const drawn = raw.flatMap(([t, s], k) => {
    const h = heights[s] ?? null;
    return h === null ? [] : [{ k, pt: [t, h, s] satisfies Pt }];
  });
  const on = (a: Pt, b: Pt, t: number): Pt => [
    t,
    a[0] === b[0] ? a[1] : a[1] + ((b[1] - a[1]) * (t - a[0])) / (b[0] - a[0]),
    a[2],
  ];
  const cut: Pt[] = [];
  const push = (q: Pt): void => {
    const l = cut[cut.length - 1];
    if (l?.[0] !== q[0] || l[1] !== q[1]) cut.push(q);
  };
  for (let n = 1; n < drawn.length; n++) {
    const a = item(drawn, n - 1),
      b = item(drawn, n);
    if (b.k <= from || a.k >= to) continue;
    push(a.k < from ? on(a.pt, b.pt, item(raw, from)[0]) : a.pt);
    push(b.k > to ? on(a.pt, b.pt, item(raw, to)[0]) : b.pt);
  }
  return cut;
}
