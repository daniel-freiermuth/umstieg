// What to do when a change is missed, for connections the page found itself
// (best.ts): as the server's src/fallback.rs, over the network it sends, which
// holds the stretches of every fallback of every change on it.

import { type Connection, type Limits, type Step, type Stretch, alightAt, boardAt, everyConnection } from "./best";
import { type Clock, type Drawn, duration, piecesOf, route, signature } from "./connections";
import { item } from "./util";

/** How long after a missed train fallbacks may leave, and how they're bounded. */
const WINDOW = 2 * 60 * 60e3;
const EXTRA = 60 * 60e3;
const CHANGES = 3;
const SHOWN = 3;

/** A change of a connection: where the train arrived and when, the next departure, when one is ready for it. */
export interface Transfer {
  station: number;
  arrival: number;
  departure: number;
  ready: number;
}

/** A connection's changes, in order (a walk between trains counts into `ready`). */
export function transfers(c: Connection): Transfer[] {
  const out: Transfer[] = [];
  let arrived: [station: number, at: number] | null = null;
  let walked = 0;
  for (const s of c.steps) {
    if (s.kind === "walk") {
      walked += s.end - s.start;
      continue;
    }
    const board = item(s.calls, 0),
      alight = item(s.calls, s.calls.length - 1);
    if (arrived !== null && board.dep !== null)
      out.push({ station: arrived[0], arrival: arrived[1], departure: board.dep, ready: arrived[1] + walked });
    arrived = alight.arr === null ? null : [alight.station, alight.arr];
    walked = 0;
  }
  return out;
}

/** A fallback as the page shows it: the line, its pieces, and when it lands. */
export interface Alternative {
  /** Where the change is missed. */
  station: number;
  line: string;
  pieces: Drawn[];
  /** Arrival at the destination: the plan's if the plan is rejoined. */
  lands: number;
}

/** What the fallbacks are searched over, and how they are written. */
export interface Ground {
  pieces: readonly Stretch[];
  network: readonly number[];
  stations: number;
  limits: Limits;
  names: readonly string[];
  trips: readonly string[];
  clock: Clock;
}

/**
 * Fallbacks of the connections the page finds itself, one per change: the
 * candidates per (station, earliest departure) found once.
 */
export class Fallbacks {
  private readonly cache = new Map<string, Connection[]>();
  constructor(private readonly ground: Ground) {}

  /** Every connection from `station` within the window after the missed train, at most an hour above the fastest. */
  private candidates(station: number, missed: number, ready: number): Connection[] {
    const after = Math.max(missed + 60e3, ready);
    const key = `${String(station)}@${String(after)}`;
    const known = this.cache.get(key);
    if (known) return known;
    const g = this.ground;
    const limits: Limits = {
      ...g.limits,
      origin: station,
      from: after,
      until: after + WINDOW,
      maxDuration: Infinity,
      maxChanges: CHANGES,
      // What to do once the change is missed: only what still works.
      missedBy: 0,
      keepDetours: false,
    };
    const all = everyConnection(g.pieces, g.network, g.stations, limits);
    const fastest = Math.min(...all.map((c) => c.arrival - c.departure));
    const found = all.filter((c) => c.arrival - c.departure <= fastest + EXTRA);
    // Stryker disable next-line all: a cache, the same found without it.
    this.cache.set(key, found);
    return found;
  }

  /**
   * The best fallbacks if the train after change `change` of `plan` is missed:
   * ranked by what happens until the plan is rejoined (from there it goes
   * on as planned), then changes and buffer; the same line once.
   */
  forChange(plan: Connection, change: number): Alternative[] {
    const g = this.ground;
    const t = item(transfers(plan), change);
    const rejoin = rejoins(plan);
    // From where the train arrived, from the arrival on: after a change with a
    // walk, fallbacks walk there too, or elsewhere. And from where the missed
    // train left, once walked there: a walk starting there counts only from after the missed
    // departure, but the traveller already stands there, and may leave right after it.
    let candidates = this.candidates(t.station, t.departure, t.arrival);
    const rides = plan.steps.filter((s) => s.kind === "ride");
    const boardsAt = item(item(rides, change + 1).calls, 0).station;
    // Stryker disable next-line ConditionalExpression: the same without it, from one station the search lists each chain once.
    if (boardsAt !== t.station) {
      // From there, only what boards a train right there, and doesn't come
      // back for what is listed from where the train arrived: the fallbacks
      // are shown from there, so a walk first would be shown as one from
      // there, and the rest is a second copy.
      const listed = new Set(candidates.map((c) => way(c.steps)));
      const fromBoarding = this.candidates(boardsAt, t.departure, t.ready).filter(
        (c) => c.steps[0]?.kind === "ride" && !ridesBack(c, listed),
      );
      candidates = once([...candidates, ...fromBoarding]);
    }
    const ranked = candidates.map((c) => {
      const { back, changes, slack, lands } = standing(c, rejoin, plan.arrival);
      // A first train from where the missed one left is named with its station.
      const line = route(c, g.names, g.trips, g.limits.minTransfer, g.clock, back, t.station);
      return { c, back, changes, slack, lands, line };
    });
    ranked.sort((a, b) => a.lands - b.lands || a.changes - b.changes || b.slack - a.slack || byText(a.line, b.line));
    // The same line is the same route up to the plan (and so ranked alike): once.
    const seen = new Set<string>();
    const shown = ranked.filter((r) => !seen.has(r.line) && seen.add(r.line)).slice(0, SHOWN);
    return shown.map((r) => {
      // Drawn from the missed departure on, or, for a missed change, from when
      // the traveller is where the fallback starts: where the train arrived from
      // the arrival, where the missed train left once walked there.
      const first = r.c.steps[0];
      const there = first?.kind === "ride" && item(first.calls, 0).station !== t.station ? t.ready : t.arrival;
      const since = Math.max(t.departure, there);
      return {
        station: t.station,
        line: r.line,
        lands: r.lands,
        pieces: piecesOf(r.c, g.names, g.trips, g.limits.minTransfer, g.clock, {
          start: [t.station, since],
          until: r.back,
        }),
      };
    });
  }
}

/** The same trains and walks, as the server's steps: a walk whenever it is taken (rides keep their calls' times). */
const way = (steps: readonly Step[]): string => signature(steps.map((s) => ({ ...s, start: 0, end: 0 })));

/**
 * Whether `c` goes on as one of the `listed` connections (by `way`) does,
 * after a train or a walk, or from a stop ridden through after a change: the
 * listed ones leave where the train arrived, so `c` came back there. Riding
 * through on the first train is that connection boarded elsewhere (`once`).
 */
function ridesBack(c: Connection, listed: ReadonlySet<string>): boolean {
  const listedOn = (steps: readonly Step[]): boolean => listed.has(way(steps));
  let changed = false;
  for (const [i, s] of c.steps.entries()) {
    const rest = c.steps.slice(i + 1);
    if (s.kind === "ride") {
      // Stryker disable next-line MethodExpression: the same over every call: on from the first is checked after the step before, from the last never listed.
      const through = s.calls.slice(1, -1);
      if (changed && through.some((_, k) => listedOn([{ ...s, calls: s.calls.slice(k + 1) }, ...rest]))) return true;
      changed = true;
    }
    if (listedOn(rest)) return true;
  }
  return false;
}

/**
 * Each fallback once: the same connection from its first train on, boarded
 * without a walk, is one fallback, boarded where it calls first. Walked to,
 * it is one of its own, unless also boarded at that call without a walk.
 */
function once(found: readonly Connection[]): Connection[] {
  const copies = found.flatMap((c) => {
    const first = c.steps.findIndex((s) => s.kind === "ride");
    const ride = c.steps[first];
    if (ride?.kind !== "ride") return [];
    const end = item(ride.calls, ride.calls.length - 1);
    // The train, where it is left and all after it; where it is boarded:
    // of the same train left at the same call, the more calls, the earlier.
    const key = `${String(ride.trip)}>${String(end.station)}@${String(end.arr)} ${way(c.steps.slice(first + 1))}`;
    return [{ c, key, board: ride.calls.length, walked: first > 0 }];
  });
  // Earliest boarding first: of the copies in a slot, the first stays.
  copies.sort((a, b) => b.board - a.board);
  const direct = new Set(copies.filter((x) => !x.walked).map((x) => `${x.key}|${String(x.board)}`));
  const slots = new Set<string>();
  return copies
    .filter((x) => {
      const slot = x.walked ? `${x.key}|${String(x.board)}` : x.key;
      if (slots.has(slot) || (x.walked && direct.has(slot))) return false;
      slots.add(slot);
      return true;
    })
    .map((x) => x.c);
}

/**
 * Catching a train of the plan (also the missed one, further on) before the
 * plan leaves it ends the detour: train → when the plan leaves it. The
 * plan's earlier trains were left before the change, so catching them again
 * never counts.
 */
export function rejoins(plan: Connection): Map<number, number> {
  return new Map(plan.steps.flatMap((s) => (s.kind === "ride" ? [[s.trip, alightAt(s)]] : [])));
}

/**
 * How a fallback ranks: the ride back on the plan (if any), the changes and
 * worst buffer until then, and when it lands (the plan's arrival once back).
 */
export function standing(
  c: Connection,
  rejoin: ReadonlyMap<number, number>,
  planned: number,
): { back: number | null; changes: number; slack: number; lands: number } {
  const rides = c.steps.flatMap((s) => (s.kind === "ride" ? [s] : []));
  const at = rides.findIndex((r) => boardAt(r) < (rejoin.get(r.trip) ?? -Infinity));
  const back = at < 0 ? null : at;
  const changes = back ?? rides.length - 1;
  // None without a change: all alike then.
  const slack = Math.min(
    ...transfers(c)
      .slice(0, changes)
      .map((x) => x.departure - x.ready),
  );
  return { back, changes, slack, lands: back === null ? c.arrival : planned };
}

/** Text order as the server's (by code point), for ties. */
export const byText = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0);

/** "13:40 CET (+59m)": when a fallback lands, and how much later than planned. */
export const arrive = (lands: number, planned: number, clock: Clock): string =>
  `${clock(lands)} (+${duration(Math.max(0, lands - planned))})`;
