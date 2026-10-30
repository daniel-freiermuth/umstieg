// What is reachable through a point of the plot, without the DOM: the
// network's stretches (rides, stays, walks: everything on some connection
// within the limits), the listed connections passing them, and the
// transitive closure over them.

import type { Kind } from "./data";
import type { Interval, Pt } from "./geometry";
import { entry, item } from "./util";

/** A reachable stretch of a piece: [piece, from, to]. */
export type Segment = [piece: number, from: number, to: number];

/** Where a hover target lies: a stretch of a piece, or a moment at a station. */
export type Spot = { piece: number; from: number; to: number } | { station: number; t: number };

/**
 * Whether a highlight's `segments` cover `spot`: a stretch shares time with a
 * segment of its piece (an instant one lies in it); a moment at a station
 * lies in a segment of a piece staying there then (a wait, a standing train).
 */
export function covers(segments: readonly Segment[], lines: readonly Line[], spot: Spot): boolean {
  if ("piece" in spot) {
    const { piece, from, to } = spot;
    return segments.some(([pi, a, b]) => pi === piece && (from === to ? a <= from && from <= b : a < to && b > from));
  }
  const { station, t } = spot;
  return segments.some(([pi, a, b]) => {
    if (t < a || t > b) return false;
    const pts = item(lines, pi).pts;
    return pts.some((q, k) => {
      const p = pts[k - 1];
      return p?.[2] === station && q[2] === station && p[0] <= t && t <= q[0];
    });
  });
}

/** A stretch of time at a station: waiting, or in a train standing there (`trip`). */
export interface Span {
  from: number;
  to: number;
  trip?: number;
}
/** A listed connection at a station for a stretch of time. */
export interface Presence {
  from: number;
  to: number;
  chain: number;
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

/**
 * Stretches reachable through all of several highlights: per piece, the
 * intersection of their time intervals (each list merged, as reach returns
 * them). None for no highlights.
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

/** One train's stop-to-stop stretch (or standing at a stop) of the network, and the listed connections riding it. */
export interface Ride {
  trip: number;
  pts: [Pt, Pt];
  /** The network piece it is part of. */
  piece: number;
  chains: Set<number>;
}
/** The plot's network: everything on some connection, and the listed connections in it. */
export interface Network {
  /** Per train and stop-to-stop stretch. */
  rides: Ride[];
  /** Per station: everything flat there (waits, standing trains). */
  stays: Map<number, { y: number; spans: Span[] }>;
  /** Wait pieces of the network. */
  waits: number[];
  /** Walk pieces of the network, with the listed connections walking them. */
  walks: Map<number, Set<number>>;
  /** The listed connections at a station at a time (waiting or in a standing train). */
  chainsAt: (station: number, t: number) => number[];
  /** Per station: every listed connection with a point there. */
  touching: Map<number, Set<number>>;
  /** Everything reachable from a source, transitively over all rides and walks. */
  reach: (src: Source) => Segment[];
}

// Whether a known time v is at most (at least) t; unknown is neither.
const atMost = (v: number | undefined, t: number): boolean => v !== undefined && v <= t;
const atLeast = (v: number | undefined, t: number): boolean => v !== undefined && v >= t;

const stretchKey = (trip: number, a: Pt, b: Pt): string =>
  `${String(trip)}:${String(a[0])}:${String(a[2])}:${String(b[0])}:${String(b[2])}`;

/**
 * The network's smallest units, each drawn once and used as hover target:
 *   rides: one per train and stop-to-stop stretch of the network's rides,
 *   stays: per station, its waits and standing trains,
 *   walks: per walk.
 * `base` are the network's pieces (everything on some connection within the
 * limits); `chains` the listed connections, visible or not, which only say
 * who passes where.
 */
export function network(
  pieces: readonly Line[],
  base: readonly number[],
  chains: readonly Route[],
  visible: (chain: number) => boolean,
): Network {
  const rides = new Map<string, Ride>(),
    waits: number[] = [],
    walks = new Map<number, Set<number>>(),
    stays = new Map<number, { y: number; spans: Span[] }>();
  const stay = (s: number, y: number, span: Span): void => {
    entry(stays, s, () => ({ y, spans: [] })).spans.push(span);
  };
  for (const pi of base) {
    const p = item(pieces, pi);
    // Rides carry their train, waits and walks none.
    if (p.trip !== null) {
      for (let k = 1; k < p.pts.length; k++) {
        const a = item(p.pts, k - 1),
          b = item(p.pts, k);
        rides.set(stretchKey(p.trip, a, b), { trip: p.trip, pts: [a, b], piece: pi, chains: new Set() });
        if (a[2] === b[2]) stay(a[2], a[1], { from: a[0], to: b[0], trip: p.trip });
      }
    } else if (p.kind === "wait") {
      const first = p.pts[0],
        last = p.pts[p.pts.length - 1];
      if (!first || !last) continue;
      waits.push(pi);
      stay(first[2], first[1], { from: first[0], to: last[0] });
    } else walks.set(pi, new Set());
  }

  // Who of the listed connections passes where.
  const present = new Map<number, Presence[]>(),
    touching = new Map<number, Set<number>>();
  chains.forEach((c, i) => {
    if (!visible(i)) return;
    for (const pi of c.pieces) {
      const p = item(pieces, pi);
      for (const [, , s] of p.pts) entry(touching, s, () => new Set()).add(i);
      if (p.trip !== null) {
        for (let k = 1; k < p.pts.length; k++) {
          const a = item(p.pts, k - 1),
            b = item(p.pts, k);
          rides.get(stretchKey(p.trip, a, b))?.chains.add(i);
          if (a[2] === b[2]) entry(present, a[2], () => []).push({ from: a[0], to: b[0], chain: i });
        }
      } else if (p.kind === "wait") {
        const first = p.pts[0],
          last = p.pts[p.pts.length - 1];
        if (first && last) entry(present, first[2], () => []).push({ from: first[0], to: last[0], chain: i });
      } else walks.get(pi)?.add(i);
    }
  });
  const chainsAt = (station: number, t: number): number[] => [
    ...new Set((present.get(station) ?? []).filter((w) => w.from <= t && t <= w.to).map((w) => w.chain)),
  ];

  // Reachability is transitive over the whole network: stop-to-stop rides
  // (also standing at a stop) and walks, so changing between two trains no
  // listed connection changes between counts too.
  const all = [...rides.values()];
  // Stretches grouped by departure (latest arrival first, backward).
  const groups = (key: (r: Ride) => number, later: boolean): Ride[][] => {
    const by = new Map<number, Ride[]>();
    for (const r of all) entry(by, key(r), () => []).push(r);
    return [...by].sort((x, y) => (later ? y[0] - x[0] : x[0] - y[0])).map(([, rs]) => rs);
  };
  const byDep = groups((r) => r.pts[0][0], false),
    byArr = groups((r) => r.pts[1][0], true);
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
  // Earliest arrival forward, latest departure backward; what they reach,
  // as stretches of the network's pieces.
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
    const raw = new Map<number, Interval[]>();
    const add = (pi: number, a: number, b: number): void => {
      entry(raw, pi, () => []).push([a, b]);
    };
    // On a train (since, until) every later (earlier) stretch of it counts.
    // Stretches taking no time (a train standing, squeezed live times) can
    // lead onto others at the same time in any order: such groups repeat
    // until nothing more is reached. On a train, its later (earlier)
    // stretches count from a boarding (alighting) on, also across stretches
    // no connection rides. In time order the last one set is the earliest
    // (latest) that matters: the hovered train's own stretches before (after)
    // its boarding are reached the other way.
    const boarded = new Map<number, number>(src.ride ? [[src.ride.trip, src.ride.from]] : []);
    const ahead = new Set<Ride>(),
      behind = new Set<Ride>();
    for (const group of byDep) {
      for (let again = true; again;) {
        again = false;
        for (const r of group) {
          const [a, b] = r.pts;
          const since = boarded.get(r.trip);
          if (ahead.has(r) || !((since !== undefined && a[0] >= since) || atMost(ea.get(a[2]), a[0]))) continue;
          boarded.set(r.trip, a[0]);
          early(b[2], b[0]);
          ahead.add(r);
          // Stryker disable next-line ConditionalExpression: repeating anyway only costs a pass.
          again ||= b[0] === a[0];
        }
      }
    }
    const alighted = new Map<number, number>(src.ride ? [[src.ride.trip, src.ride.until]] : []);
    for (const group of byArr) {
      for (let again = true; again;) {
        again = false;
        for (const r of group) {
          const [a, b] = r.pts;
          const until = alighted.get(r.trip);
          if (behind.has(r) || !((until !== undefined && b[0] <= until) || atLeast(ld.get(b[2]), b[0]))) continue;
          alighted.set(r.trip, b[0]);
          late(a[2], a[0]);
          behind.add(r);
          // Stryker disable next-line ConditionalExpression: repeating anyway only costs a pass.
          again ||= b[0] === a[0];
        }
      }
    }
    for (const r of new Set([...ahead, ...behind])) add(r.piece, r.pts[0][0], r.pts[1][0]);
    // Waits from when one is there on (up to when one has to leave).
    for (const pi of waits) {
      const p = item(pieces, pi);
      const first = p.pts[0],
        last = p.pts[p.pts.length - 1];
      if (!first || !last) continue;
      const [from, , s] = first,
        to = last[0];
      const there = ea.get(s),
        by = ld.get(s);
      if (there !== undefined && to >= there) add(pi, Math.max(from, there), to);
      if (by !== undefined && from <= by) add(pi, from, Math.min(to, by));
    }
    for (const pi of walks.keys()) {
      const p = item(pieces, pi);
      const a = p.pts[0],
        b = p.pts[p.pts.length - 1];
      if (!a || !b) continue;
      if (atMost(ea.get(a[2]), a[0]) || atLeast(ld.get(b[2]), b[0])) add(pi, a[0], b[0]);
    }
    const out: Segment[] = [];
    for (const [pi, list] of raw) for (const [a, b] of mergeIntervals(list)) out.push([pi, a, b]);
    return out;
  };
  return { rides: all, stays, waits, walks, chainsAt, touching, reach };
}
