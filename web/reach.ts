// What is reachable through a point of the plot, without the DOM: the shared
// stretches of the visible connections (rides, stays, walks) and the
// transitive closure over them.

import type { Kind } from "./data";
import type { Interval, Pt } from "./geometry";
import { entry, item } from "./util";

/** A reachable stretch of a piece: [piece, from, to]. */
export type Segment = [piece: number, from: number, to: number];

/**
 * A stretch of time at a station within a connection: piece position j, for
 * standing trains point k.
 */
export interface Span {
  from: number;
  to: number;
  chain: number;
  j: number;
  trip?: number;
  k?: number;
}

/** A reach highlight: `key` identifies it, `with` are the connections it is about. */
export interface Reach {
  key: string;
  with?: Set<number>;
  segments: Segment[];
}

/** A piece as reachability sees it. */
export interface Line {
  kind: Kind;
  warn: boolean;
  trip: number | null;
  pts: readonly Pt[];
}
/** A connection: its pieces by index. */
export interface Route {
  pieces: readonly number[];
}

/** Spans ({ from, to }) merged into continuous runs [from, to]; touching ones join. */
export function runsOf(spans: readonly { from: number; to: number }[]): Interval[] {
  return mergeIntervals(spans.map((w): Interval => [w.from, w.to]));
}

/** Intervals merged into sorted, disjoint runs; touching ones join. */
export function mergeIntervals(list: readonly Interval[]): Interval[] {
  const runs: Interval[] = [];
  for (const [a, b] of [...list].sort((x, y) => x[0] - y[0])) {
    const last = runs[runs.length - 1];
    if (last && a <= last[1]) last[1] = Math.max(last[1], b);
    else runs.push([a, b]);
  }
  return runs;
}

/**
 * The spans of a run [from, to] covering the hovered time, clamped into the
 * run. The highlight is the whole run: highlighting only the covering spans
 * made one line look like several waits (Mannheim Hbf until 08:03 before
 * 06:58, until 10:03 after).
 */
export function stayAt<S extends { from: number; to: number }>(
  spans: readonly S[],
  from: number,
  to: number,
  time: number,
  y: number,
  s: number,
): { t: number; here: S[]; pts: Pt[] } {
  const t = Math.min(to, Math.max(from, time));
  const here = spans.filter((w) => w.from <= t && t <= w.to);
  return {
    t,
    here,
    pts: [
      [from, y, s],
      [to, y, s],
    ],
  };
}

/** A station-time to reach through: spans at a station, the time, and which halves. */
export interface Through {
  spans: readonly Span[];
  t: number;
  back: boolean;
  fwd: boolean;
}

// Position in a connection: (piece position, point), and the cut time.
type Cut = [j: number, k: number, t: number];

/**
 * Reachability through station-times: for each item, every connection there
 * arriving by t, up to the station (back), and every one leaving from t on,
 * from the station (fwd). Later departures count too: waiting longer is
 * always possible.
 * Per connection only the earliest cut forward and the latest backward
 * matter. A wait at piece j belongs to both halves; a train standing there is
 * cut at the stop (points k-1, k).
 * The stay at the cut is clipped to the reachable time: from t on forward, up
 * to t backward, so waits don't light up before anything reachable arrives.
 * Returns per piece the reachable stretches as time intervals [piece, from,
 * to] (pieces run forward in time), merged, so highlights of several picks
 * can be intersected exactly.
 */
export function reachSegments(pieces: readonly Line[], chains: readonly Route[], items: readonly Through[]): Segment[] {
  const from = new Map<number, Cut>(),
    upTo = new Map<number, Cut>();
  // Cuts in order of position, then time (the same stay reached at two times).
  const before = (a: Cut, b: Cut): boolean => {
    // Stryker disable next-line EqualityOperator: <= only differs for equal cuts, and replacing one by an equal one changes nothing.
    const sooner = a[2] < b[2];
    return a[0] < b[0] || (a[0] === b[0] && (a[1] < b[1] || (a[1] === b[1] && sooner)));
  };
  const last = (pi: number): number => item(pieces, pi).pts.length - 1;
  for (const { spans, t, back, fwd } of items)
    for (const w of spans) {
      if (fwd && w.to >= t) {
        const p: Cut = w.k === undefined ? [w.j, 0, t] : [w.j, w.k - 1, t],
          q = from.get(w.chain);
        if (!q || before(p, q)) from.set(w.chain, p);
      }
      if (back && w.from <= t) {
        const p: Cut = w.k === undefined ? [w.j, last(item(item(chains, w.chain).pieces, w.j)), t] : [w.j, w.k, t],
          q = upTo.get(w.chain);
        if (!q || before(q, p)) upTo.set(w.chain, p);
      }
    }
  const raw = new Map<number, Interval[]>();
  // Points k0..k1 of a piece; the end at a cut (always flat: a wait, or a
  // train standing) is clipped to the cut time.
  const add = (pi: number, k0: number, k1: number, c0: number | null = null, c1: number | null = null): void => {
    const s = item(pieces, pi).pts;
    if (!s.length) return;
    let a = item(s, k0)[0],
      b = item(s, k1)[0];
    if (c0 !== null && k0 < k1) a = Math.min(Math.max(a, c0), item(s, k0 + 1)[0]);
    if (c1 !== null && k1 > k0) b = Math.max(Math.min(b, c1), item(s, k1 - 1)[0]);
    entry(raw, pi, () => []).push([a, b]);
  };
  for (const [chain, [j, k0, t]] of from) {
    const ps = item(chains, chain).pieces;
    add(item(ps, j), k0, last(item(ps, j)), t, null);
    for (let q = j + 1; q < ps.length; q++) add(item(ps, q), 0, last(item(ps, q)));
  }
  for (const [chain, [j, k1, t]] of upTo) {
    const ps = item(chains, chain).pieces;
    for (let q = 0; q < j; q++) add(item(ps, q), 0, last(item(ps, q)));
    add(item(ps, j), 0, k1, null, t);
  }
  const out: Segment[] = [];
  for (const [pi, list] of raw) for (const [a, b] of mergeIntervals(list)) out.push([pi, a, b]);
  return out;
}

/**
 * Stretches reachable through all of several highlights: per piece, the
 * intersection of their time intervals (each list merged, as reachSegments
 * returns them). None for no highlights.
 */
export function intersectReach(segmentLists: readonly (readonly Segment[])[]): Segment[] {
  const maps = segmentLists.map((list) => {
    const m = new Map<number, Interval[]>();
    for (const [pi, a, b] of list) entry(m, pi, () => []).push([a, b]);
    return m;
  });
  const [head, ...rest] = maps;
  if (!head) return [];
  const out: Segment[] = [];
  for (const [pi, first] of head) {
    let cur = first;
    for (const m of rest) {
      const other = m.get(pi);
      if (!other) {
        cur = [];
        break;
      }
      const next: Interval[] = [];
      for (const [a, b] of cur)
        for (const [c, d] of other) {
          const lo = Math.max(a, c),
            hi = Math.min(b, d);
          if (lo <= hi) next.push([lo, hi]);
        }
      cur = next;
    }
    for (const [a, b] of cur) out.push([pi, a, b]);
  }
  return out;
}

/**
 * Where reachability starts: being at stations at times (`at`: both ways;
 * `back`: only arrived there by then; `fwd`: only leaving from then on), or on
 * `ride.trip` (riding on from departure `from`, having ridden up to arrival
 * `until`).
 */
export interface Source {
  at: [station: number, t: number][];
  back?: [station: number, t: number][];
  fwd?: [station: number, t: number][];
  ride?: { trip: number; from: number; until: number };
}

/** One train's stop-to-stop stretch (or standing at a stop), shared by every connection riding it. */
export interface Ride {
  trip: number;
  pts: [Pt, Pt];
  chains: Set<number>;
}
/** Waits at a station: their spans and the continuous runs they make. */
export interface Wait {
  y: number;
  spans: Span[];
  runs: Interval[];
}
/** The smallest shared units of the visible connections. */
export interface Network {
  /** Per train and stop-to-stop stretch, in order of first appearance. */
  rides: Ride[];
  /** Per station: everything flat there (waits, standing trains). */
  stays: Map<number, { y: number; spans: Span[] }>;
  waits: Wait[];
  /** Walk pieces with the connections walking them. */
  walks: Map<number, Set<number>>;
  /** Wait pieces below the minimum transfer time (or missed). */
  warns: number[];
  /** Per station: every connection with a point there. */
  touching: Map<number, Set<number>>;
  /** Everything reachable from a source, transitively over all rides and walks. */
  reach: (src: Source) => Segment[];
}

// Whether a known time v is at most (at least) t; unknown is neither.
const atMost = (v: number | undefined, t: number): boolean => v !== undefined && v <= t;
const atLeast = (v: number | undefined, t: number): boolean => v !== undefined && v >= t;

/**
 * The smallest shared units of the visible connections, each drawn once and
 * used as hover target, so every connection passing through counts, wherever
 * it boards or leaves:
 *   rides: one per train and stop-to-stop stretch,
 *   stays: per station and continuous stay, waiting or in a standing train,
 *   walks: per walk.
 */
export function network(
  pieces: readonly Line[],
  chains: readonly Route[],
  visible: (chain: number) => boolean,
): Network {
  const rides = new Map<string, Ride>(),
    waits = new Map<number, Wait>(),
    walks = new Map<number, Set<number>>(),
    warns = new Set<number>();
  // Everything flat at a station (waits, trains standing there) is one target
  // per continuous stay, so nothing stacks. Spans remember where in their
  // connection they are (piece position j, for standing trains point k) for
  // the reachability highlight.
  const stays = new Map<number, { y: number; spans: Span[] }>();
  const stay = (s: number, y: number, span: Span): void => {
    entry(stays, s, () => ({ y, spans: [] })).spans.push(span);
  };
  chains.forEach((c, i) => {
    if (!visible(i)) return;
    c.pieces.forEach((pi, j) => {
      const p = item(pieces, pi);
      if (p.kind === "ride") {
        if (p.trip === null) return;
        const trip = p.trip;
        for (let k = 1; k < p.pts.length; k++) {
          const a = item(p.pts, k - 1),
            b = item(p.pts, k);
          const key = `${String(trip)}:${String(a[0])}:${String(a[2])}:${String(b[0])}:${String(b[2])}`;
          entry(rides, key, () => ({ trip, pts: [a, b], chains: new Set() })).chains.add(i);
          if (a[2] === b[2]) stay(a[2], a[1], { from: a[0], to: b[0], chain: i, trip, j, k });
        }
      } else if (p.kind === "wait") {
        const first = p.pts[0],
          last = p.pts[p.pts.length - 1];
        if (!first || !last) return;
        const s = first[2],
          span = { from: first[0], to: last[0], chain: i, j };
        entry(waits, s, () => ({ y: first[1], spans: [], runs: [] })).spans.push(span);
        stay(s, first[1], span);
        if (p.warn) warns.add(pi);
      } else entry(walks, pi, () => new Set()).add(i);
    });
  });
  // Overlapping waits at a station merge into one line per continuous stay.
  for (const w of waits.values()) w.runs = runsOf(w.spans);

  // Reachability is transitive over everything plotted: stop-to-stop rides
  // (also standing at a stop) and walks, so changing between two trains that
  // no listed connection changes between counts too (alight at Kassel, take
  // the next train through there).
  const conns = [...rides.values()].map((e) => ({
    trip: e.trip,
    from: e.pts[0][2],
    dep: e.pts[0][0],
    to: e.pts[1][2],
    arr: e.pts[1][0],
  }));
  const byDep = [...conns].sort((a, b) => a.dep - b.dep),
    byArr = [...conns].sort((a, b) => b.arr - a.arr);
  // Walks from (to) a station: where to (from), how long.
  const foot = new Map<number, [number, number][]>(),
    footBack = new Map<number, [number, number][]>();
  for (const pi of walks.keys()) {
    const p = item(pieces, pi);
    const a = p.pts[0],
      b = p.pts[p.pts.length - 1];
    if (!a || !b) continue;
    entry(foot, a[2], () => []).push([b[2], b[0] - a[0]]);
    entry(footBack, b[2], () => []).push([a[2], b[0] - a[0]]);
  }
  // Earliest arrival forward, latest departure backward.
  const reach = (src: Source): Segment[] => {
    const ea = new Map<number, number>(),
      ld = new Map<number, number>();
    const early = (x: number, t: number): void => {
      if (atMost(ea.get(x), t)) return;
      ea.set(x, t);
      for (const [y, d] of foot.get(x) ?? []) early(y, t + d);
    };
    const late = (x: number, t: number): void => {
      if (atLeast(ld.get(x), t)) return;
      ld.set(x, t);
      for (const [y, d] of footBack.get(x) ?? []) late(y, t - d);
    };
    for (const [s, t] of src.at) {
      early(s, t);
      late(s, t);
    }
    for (const [s, t] of src.back ?? []) late(s, t);
    for (const [s, t] of src.fwd ?? []) early(s, t);
    // On a train (since, until) every later (earlier) stretch of it counts,
    // even across stretches no visible connection rides.
    const boarded = new Map<number, number>(src.ride ? [[src.ride.trip, src.ride.from]] : []);
    for (const c of byDep) {
      const since = boarded.get(c.trip);
      if ((since !== undefined && c.dep >= since) || atMost(ea.get(c.from), c.dep)) {
        boarded.set(c.trip, c.dep);
        early(c.to, c.arr);
      }
    }
    const alighted = new Map<number, number>(src.ride ? [[src.ride.trip, src.ride.until]] : []);
    for (const c of byArr) {
      const until = alighted.get(c.trip);
      if ((until !== undefined && c.arr <= until) || atLeast(ld.get(c.to), c.arr)) {
        alighted.set(c.trip, c.arr);
        late(c.from, c.dep);
      }
    }
    const items: Through[] = [];
    for (const [x, t] of ea) {
      const st = stays.get(x);
      if (st) items.push({ spans: st.spans, t, back: false, fwd: true });
    }
    for (const [x, t] of ld) {
      const st = stays.get(x);
      if (st) items.push({ spans: st.spans, t, back: true, fwd: false });
    }
    return reachSegments(pieces, chains, items);
  };

  // A station on the axis: every connection touching it.
  const touching = new Map<number, Set<number>>();
  chains.forEach((c, i) => {
    if (!visible(i)) return;
    for (const pi of c.pieces) for (const [, , s] of item(pieces, pi).pts) entry(touching, s, () => new Set()).add(i);
  });
  return { rides: [...rides.values()], stays, waits: [...waits.values()], walks, warns: [...warns], touching, reach };
}
