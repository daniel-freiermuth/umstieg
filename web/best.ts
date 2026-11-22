// The best connections passing picked lines and stations, found in the
// browser over the drawn network. Every connection within the limits lies on
// that network, and searching it by the same rules as the server (window,
// travel time, changes, changes missed by a little, walks, no station twice,
// no detours, forward only) finds exactly those connections. The detour rule needs a
// train's earlier stops, known here only where the network has them.

import { item } from "./util";

/** A stop of a train: arrival and departure in ms, either missing at its ends. */
export interface Call {
  station: number;
  arr: number | null;
  dep: number | null;
}

/** What the search needs of a piece: its kind, train, (ms, station) points, stops passed only and calls. */
export interface Stretch {
  kind: "ride" | "wait" | "walk";
  trip: number | null;
  raw: readonly (readonly [number, number])[];
  /** First points of stops a ride passes without stopping (cancelled): never boarded or left there. */
  skips: readonly number[];
  /** First points of stops a ride has no departure at (set-down only): left there, never boarded. */
  setdowns: readonly number[];
  /** A ride's first and last call, as positions in its train's calls (where runs of it meet). */
  span: readonly [number, number] | null;
}

/** The search's limits, times in ms. */
export interface Limits {
  origin: number;
  destination: number;
  /** Connections leave the origin from `from` until `until`. */
  from: number;
  until: number;
  maxDuration: number;
  maxChanges: number;
  /** Changes missed by up to this much count (delays may still shift). */
  missedBy: number;
  /** Changes shorter than this are flagged; the detour rule assumes it. */
  minTransfer: number;
  /** Keep connections boarding a train they could have boarded at an earlier stop. */
  keepDetours: boolean;
  /**
   * With forward only, the least remaining travel time from each station in ms
   * (-Infinity where the destination is out of reach, as the server orders it):
   * a ride is left only where this is less than where it was boarded. The
   * server's bounds, so stations added after its search are missing.
   */
  lowerBound: readonly number[] | null;
}

/** What a connection must pass: a picked stretch, stay, walk or station. */
export type Need =
  | { kind: "ride"; trip: number; from: number; at: number; to: number; until: number }
  | { kind: "stay"; station: number; at: number }
  | { kind: "walk"; from: number; to: number; at: number }
  | { kind: "station"; station: number };

export type Step =
  | {
      kind: "ride";
      trip: number;
      calls: Call[];
      /** Positions in `calls` it only passes (cancelled). */
      passes: readonly number[];
    }
  | { kind: "walk"; from: number; to: number; start: number; end: number };

export interface Connection {
  steps: Step[];
  departure: number;
  arrival: number;
  changes: number;
  /** Worst change buffer in ms (negative: missed); null without a change. */
  slack: number | null;
}

type Ride = Extract<Step, { kind: "ride" }>;
/** When a ride leaves its first stop and reaches its last: rides always do. */
export function boardAt(s: Ride): number {
  const t = item(s.calls, 0).dep;
  // Stryker disable next-line StringLiteral: unreachable, the search boards only where trains leave.
  if (t === null) throw new Error("a ride leaves where it is boarded");
  return t;
}
export function alightAt(s: Ride): number {
  const t = item(s.calls, s.calls.length - 1).arr;
  // Stryker disable next-line StringLiteral: unreachable, the search alights only where trains arrive.
  if (t === null) throw new Error("a ride arrives where it is left");
  return t;
}

/** A worst buffer this long is enough: longer ones don't count as better. */
export const ENOUGH_BUFFER = 90 * 60e3;

/** The calls of a train's run from its points: arrival and departure where it stands. */
export function callsOf(raw: readonly (readonly [number, number])[]): Call[] {
  const calls: Call[] = [];
  for (const [t, s] of raw) {
    const last = calls[calls.length - 1];
    if (last?.station === s) last.dep = t;
    else calls.push({ station: s, arr: t, dep: t });
  }
  const first = calls[0],
    end = calls[calls.length - 1];
  if (first) first.arr = null;
  if (end) end.dep = null;
  return calls;
}

/** Positions in `callsOf(raw)` of the calls whose first point is in `skips`. */
function skippedCalls(raw: readonly (readonly [number, number])[], skips: readonly number[]): Set<number> {
  const out = new Set<number>();
  let call = -1;
  raw.forEach(([, s], i) => {
    // As callsOf: points at the station of the one before belong to its call.
    if (raw[i - 1]?.[1] !== s) call++;
    if (skips.includes(i)) out.add(call);
  });
  return out;
}

/** Departure, arrival, changes and buffer (capped) of a connection: what "best" compares. */
interface Key {
  dep: number;
  arr: number;
  changes: number;
  slack: number;
}
const keyOf = (c: Connection): Key => ({
  dep: c.departure,
  arr: c.arrival,
  changes: c.changes,
  slack: Math.min(c.slack ?? ENOUGH_BUFFER, ENOUGH_BUFFER),
});
/** `a` beats `b`: no worse in any of the four, better in one. */
const beats = (a: Key, b: Key): boolean =>
  (a.dep !== b.dep || a.arr !== b.arr || a.changes !== b.changes || a.slack !== b.slack) &&
  a.dep >= b.dep &&
  a.arr <= b.arr &&
  a.changes <= b.changes &&
  a.slack >= b.slack;

/** Times a need is tied to: once the connection is past them, it can't be met. */
const needTime = (n: Need): number | null => (n.kind === "station" ? null : n.at);

/** Whether the whole connection `steps` meets `n`. */
export function meets(steps: readonly Step[], n: Need): boolean {
  switch (n.kind) {
    case "ride":
      return steps.some(
        (s) =>
          s.kind === "ride" &&
          s.trip === n.trip &&
          s.calls.some((c, k) => {
            const next = s.calls[k + 1];
            return c.station === n.from && c.dep === n.at && next?.station === n.to && next.arr === n.until;
          }),
      );
    case "walk":
      return steps.some((s) => s.kind === "walk" && s.from === n.from && s.to === n.to && s.start === n.at);
    case "station":
      return steps.some((s) =>
        s.kind === "walk"
          ? s.from === n.station || s.to === n.station
          : s.calls.some((c, k) => c.station === n.station && !s.passes.includes(k)),
      );
    case "stay":
      return presence(steps).some(([s, a, b]) => s === n.station && a <= n.at && n.at <= b);
  }
}

/**
 * Where and when a connection stays: in a train standing at a stop, and
 * between arriving somewhere (by train or on foot) and the next departure
 * from there; a missed change from the departure to the arrival.
 */
function presence(steps: readonly Step[]): [station: number, from: number, to: number][] {
  const out: [number, number, number][] = [];
  let here: [number, number] | null = null;
  for (const s of steps) {
    if (s.kind === "walk") {
      if (here) out.push([here[0], here[1], s.start]);
      here = [s.to, s.end];
      continue;
    }
    const first = s.calls[0],
      last = s.calls[s.calls.length - 1];
    if (!first || !last) continue;
    if (here && first.dep !== null) out.push([here[0], Math.min(here[1], first.dep), Math.max(here[1], first.dep)]);
    // Passing a stop (cancelled) is no standing there.
    s.calls.slice(1, -1).forEach((c, k) => {
      if (c.arr !== null && c.dep !== null && !s.passes.includes(k + 1)) out.push([c.station, c.arr, c.dep]);
    });
    here = last.arr === null ? null : [last.station, last.arr];
  }
  return out;
}

interface Run {
  trip: number;
  calls: Call[];
  /** Positions in `calls` of the stops passed without stopping. */
  skips: ReadonlySet<number>;
  /** Positions in `calls` of the stops without a departure: never boarded. */
  setdowns: ReadonlySet<number>;
  span: readonly [number, number] | null;
}

/** A station the connection stopped at, for the detour rule (as the server's search keeps it). */
export interface Presence {
  station: number;
  /** Arrival; null at the origin. */
  arrival: number | null;
  /** From when trains there count as catchable (the origin: the first departure). */
  since: number;
  /** Buffer of the change made here, once the next train is chosen. */
  slackOut: number | null;
  /**
   * Where a train boarded starts the connection (the origin, or where the
   * walk from it leads): the walk from the origin before it; null elsewhere.
   */
  starts: number | null;
}

/**
 * What makes boarding a train from one earlier stop no detour after all: the
 * rest of the connection passes one of `stations`, or arrives after `by`.
 */
export interface Unless {
  stations: number[];
  by: number;
}

/**
 * Boarding a train leaving at `dep` where the connection stands (the last
 * presence) is a detour if the train called earlier (`earlier`) at a station
 * the connection stopped at before, catchable there comfortably and with at
 * least the buffer the detour's changes leave, and boarded there would have
 * got here and on as the connection does: passing no station the connection
 * had been at by then, nor one the rest of it passes (`reached`: the
 * presence a station visited was reached for, null if not visited). At the
 * origin any later departure of the train counts; after a walk from there,
 * also an earlier one, there or where the walk led, but only for a
 * connection arriving within the travel time limit from leaving the origin
 * for it. Either way only one leaving the origin within the window counts,
 * as the search lists no other. As the server's: null if it is a detour
 * whatever follows (arriving by `deadline`); otherwise, per earlier stop it
 * is a detour from unless the rest of the connection passes one or arrives
 * later, the stations the train passes from there that the connection has
 * not been at (none: no detour) and the latest arrival boarding there allows.
 */
export function detourUnless(
  presence: readonly Presence[],
  earlier: readonly Call[],
  dep: number,
  deadline: number,
  limits: Pick<Limits, "until" | "maxDuration" | "minTransfer">,
  reached: (station: number) => number | null,
): Unless[] | null {
  const unless: Unless[] = [];
  const here = presence[presence.length - 1];
  if (!here) return unless;
  let worst = here.arrival === null ? Infinity : dep - here.arrival;
  for (let stop = presence.length - 2; stop >= 0; stop--) {
    const p = item(presence, stop);
    const at = earlier.findIndex((c) => c.station === p.station);
    const caught = at < 0 ? null : item(earlier, at).dep;
    if (caught !== null && caught >= p.since) {
      const direct = p.arrival === null ? Infinity : caught - p.arrival;
      const comfortable = direct >= limits.minTransfer && direct >= Math.min(worst, p.slackOut ?? Infinity);
      // Boarding where it starts the connection, it leaves the origin a walk before the train.
      const leaves = p.starts === null ? null : caught - p.starts;
      const stations =
        comfortable && (leaves === null || leaves <= limits.until)
          ? notYetPassed(stop, earlier.slice(at + 1), reached)
          : null;
      if (stations) {
        // It may leave the origin earlier than the connection and take longer: listed only if within the limit.
        const by = leaves === null ? Infinity : leaves + limits.maxDuration;
        if (stations.length === 0 && by >= deadline) return null;
        unless.push({ stations, by });
      }
    }
    worst = Math.min(worst, p.slackOut ?? Infinity);
  }
  return unless;
}

/**
 * The stations of `calls` the connection has not been at, for a train boarded
 * at its stop `stop`; null if one is at a station it had been at by then,
 * where that ride would end (stops without a departure included).
 */
function notYetPassed(
  stop: number,
  calls: readonly Call[],
  reached: (station: number) => number | null,
): number[] | null {
  const stations: number[] = [];
  for (const c of calls) {
    const r = reached(c.station);
    if (r === null) stations.push(c.station);
    else if (r <= stop) return null;
  }
  return stations;
}

/** The earliest arrival at the destination and earliest step from a station, both lower bounds. */
type Reach = (station: number, at: number, behind: number, boardings: number) => [arrival: number, step: number];

// Stryker disable all: pruning only, the same found without it.
/**
 * Lower bounds for a partial connection at `station` at `at` with `boardings`
 * trains still to board, having ridden in from the stop `behind` (-1 after a
 * walk): when it can reach the destination and when it can take any further
 * step. Each train boarded may leave up to `missedBy` before the connection
 * gets there, so both may lie before `at`. Bounds by the same moves as the
 * search, keeping of its other rules only that a train boarded never passes
 * the stop ridden in from (been there): so a connection never does better, and
 * riding back and forth between stations, missing each change a little,
 * doesn't lower the bounds once more with every train still to board.
 */
function reach(
  departures: ReadonlyMap<number, readonly [Run, number, number][]>,
  walks: ReadonlyMap<number, ReadonlyMap<number, number>>,
  destination: number,
  missedBy: number,
  maxBoardings: number,
): Reach {
  // Per station and stop ridden in from (-1: any), its departures' bounds when
  // boarded, as the least from each departure on.
  type Level = Map<number, Map<number, [arrival: Float64Array, step: Float64Array]>>;
  const position = new Map<Run, Int32Array>();
  for (const list of departures.values())
    list.forEach(([run, k], i) => {
      const at = position.get(run) ?? new Int32Array(run.calls.length).fill(-1);
      position.set(run, at);
      at[k] = i;
    });
  // Per station, the stops rides come in from; per run, where it last calls at each station.
  const behinds = new Map<number, Set<number>>();
  const lastStop = new Map<Run, Map<number, number>>();
  for (const run of position.keys()) {
    const last = new Map<number, number>();
    run.calls.forEach((c, j) => {
      last.set(c.station, j);
      const before = run.calls[j - 1];
      if (before && c.arr !== null) behinds.set(c.station, (behinds.get(c.station) ?? new Set()).add(before.station));
    });
    lastStop.set(run, last);
  }
  const boarded = (level: Level | undefined, station: number, at: number, behind: number): [number, number] => {
    const list = departures.get(station);
    const tables = level?.get(station);
    const bounds = tables?.get(behind) ?? tables?.get(-1);
    if (!list || !bounds) return [Infinity, Infinity];
    let lo = 0,
      hi = list.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (item(list, mid)[2] < at - missedBy) lo = mid + 1;
      else hi = mid;
    }
    return [bounds[0][lo] ?? Infinity, bounds[1][lo] ?? Infinity];
  };
  const from = (level: Level | undefined, station: number, at: number, behind: number): [number, number] => {
    let [arrival, step] = boarded(level, station, at, behind);
    if (station === destination) arrival = Math.min(arrival, at);
    step = Math.min(step, at);
    for (const [to, d] of walks.get(station) ?? []) {
      const [a, s] = boarded(level, to, at + d, -1);
      arrival = Math.min(arrival, a, to === destination ? at + d : Infinity);
      step = Math.min(step, s);
    }
    return [arrival, step];
  };
  // levels[b]: with b trains still to board; the last one also for more.
  const levels: (Level | undefined)[] = [undefined];
  for (let b = 1; b <= maxBoardings; b++) {
    const prev = levels[b - 1];
    const level: Level = new Map();
    for (const [station, list] of departures) {
      const tables = new Map<number, [Float64Array, Float64Array]>();
      for (const behind of [-1, ...(behinds.get(station) ?? [])])
        tables.set(behind, [
          new Float64Array(list.length).fill(Infinity),
          new Float64Array(list.length).fill(Infinity),
        ]);
      level.set(station, tables);
    }
    for (const [run, at] of position) {
      // The bounds from getting off at each stop, and at any from there on.
      const off = run.calls.map((c, j): [number, number] => {
        if (c.arr === null) return [Infinity, Infinity];
        const [a, s] = from(prev, c.station, c.arr, run.calls[j - 1]?.station ?? -1);
        return [a, Math.min(s, c.dep ?? Infinity)];
      });
      const rest = [...off];
      for (let j = rest.length - 2; j >= 0; j--) {
        const [a, s] = item(rest, j),
          [na, ns] = item(rest, j + 1);
        rest[j] = [Math.min(a, na), Math.min(s, ns)];
      }
      const last = lastStop.get(run);
      run.calls.forEach((c, i) => {
        const p = at[i] ?? -1;
        if (p < 0 || c.dep === null) return;
        for (const [behind, bounds] of level.get(c.station) ?? []) {
          let [arrival, step] = rest[i + 1] ?? [Infinity, Infinity];
          // Up to the stop behind, if the train calls there later.
          if ((last?.get(behind) ?? -1) > i) {
            arrival = step = Infinity;
            for (let j = i + 1; item(run.calls, j).station !== behind; j++) {
              const [a, s] = item(off, j);
              arrival = Math.min(arrival, a);
              step = Math.min(step, s);
            }
          }
          bounds[0][p] = arrival;
          bounds[1][p] = Math.min(step, c.dep);
        }
      });
    }
    let same = prev !== undefined;
    for (const [station, tables] of level)
      for (const [behind, [a, s]] of tables) {
        for (let i = a.length - 2; i >= 0; i--) {
          a[i] = Math.min(a[i] ?? Infinity, a[i + 1] ?? Infinity);
          s[i] = Math.min(s[i] ?? Infinity, s[i + 1] ?? Infinity);
        }
        const p = prev?.get(station)?.get(behind);
        same &&= p !== undefined && a.every((x, i) => x === p[0][i]) && s.every((x, i) => x === p[1][i]);
      }
    if (same) break;
    levels.push(level);
  }
  return (station, at, behind, boardings) => from(levels[Math.min(boardings, levels.length - 1)], station, at, behind);
}
// Stryker restore all

/**
 * The best connections (none beaten on departure, arrival, changes and worst
 * buffer) meeting every need, latest departure first, and whether the search
 * finished within `maxNodes` steps.
 */
export function bestThrough(
  pieces: readonly Stretch[],
  network: readonly number[],
  stations: number,
  limits: Limits,
  needs: readonly Need[],
  maxNodes = 400_000,
): { found: Connection[]; complete: boolean } {
  return search(pieces, network, stations, limits, needs, maxNodes, false);
}

/** Every connection within `limits`, latest departure first (what fallbacks are chosen from). */
export function everyConnection(
  pieces: readonly Stretch[],
  network: readonly number[],
  stations: number,
  limits: Limits,
  maxNodes = 400_000,
): Connection[] {
  return search(pieces, network, stations, limits, [], maxNodes, true).found;
}

function search(
  pieces: readonly Stretch[],
  network: readonly number[],
  stations: number,
  limits: Limits,
  needs: readonly Need[],
  maxNodes: number,
  every: boolean,
): { found: Connection[]; complete: boolean } {
  const runs: Run[] = [];
  const walks = new Map<number, Map<number, number>>();
  for (const p of network.map((pi) => item(pieces, pi))) {
    // Rides carry their train; waits are where connections stood.
    if (p.trip !== null) {
      runs.push({
        trip: p.trip,
        calls: callsOf(p.raw),
        skips: skippedCalls(p.raw, p.skips),
        setdowns: skippedCalls(p.raw, p.setdowns),
        span: p.span,
      });
      continue;
    }
    // Stryker disable next-line ConditionalExpression: a wait taken for a walk leads from a station to itself, never walked (visited).
    if (p.kind !== "walk") continue;
    const [a, b] = [item(p.raw, 0), item(p.raw, p.raw.length - 1)];
    const from = walks.get(a[1]) ?? new Map<number, number>();
    walks.set(a[1], from);
    from.set(b[1], Math.min(from.get(b[1]) ?? Infinity, b[0] - a[0]));
  }
  // Every known stop per train, in time order: where it called before boarding.
  // A stop the network splits the train into runs at is one call, as the
  // server has it: the departing run's (an arriving run's last call has no departure).
  // Stryker disable next-line ArrayDeclaration: unreachable, rides always name their calls.
  const resumed = new Set(runs.flatMap((r) => (r.span ? [`${String(r.trip)}:${String(r.span[0])}`] : [])));
  const tripCalls = new Map<number, Call[]>();
  for (const run of runs) {
    const split = run.span !== null && resumed.has(`${String(run.trip)}:${String(run.span[1])}`);
    const kept = split ? run.calls.slice(0, run.calls.length - 1) : run.calls;
    // A stop without a departure, or one passed (cancelled), is no place the train could have been caught.
    const calls = kept.map((c, k) => (run.setdowns.has(k) || run.skips.has(k) ? { ...c, dep: null } : c));
    tripCalls.set(run.trip, [...(tripCalls.get(run.trip) ?? []), ...calls]);
  }
  for (const calls of tripCalls.values()) calls.sort((a, b) => (a.dep ?? a.arr ?? 0) - (b.dep ?? b.arr ?? 0));
  // Departures per station, earliest first: (run, call, time).
  const departures = new Map<number, [Run, number, number][]>();
  for (const run of runs)
    run.calls.forEach((c, k) => {
      if (c.dep === null || run.skips.has(k) || run.setdowns.has(k)) return;
      const list = departures.get(c.station) ?? [];
      departures.set(c.station, list);
      list.push([run, k, c.dep]);
    });
  for (const list of departures.values()) list.sort((a, b) => a[2] - b[2]);

  // Per station visited, 1 + the presence it was reached for (the origin 0,
  // passed stations the stop they lead to), as the server's; 0 if not visited.
  const visited = new Int32Array(stations);
  const reached = (s: number): number | null => {
    const v = visited[s] ?? 0;
    return v ? v - 1 : null;
  };
  const presence: Presence[] = [];
  const used = new Set<number>();
  const steps: Step[] = [];
  // Per train boarded, what makes boarding it no detour after all (as `detourUnless` says).
  const deferred: Unless[][] = [];
  let found: Connection[] = [];
  let nodes = 0;
  let complete = true;
  let rides = 0;
  let leave = 0;
  let slack: number | null = null;

  const record = (arrival: number): void => {
    // A detour after all: boarding a train earlier was listed, passing none of the stations.
    if (deferred.some((u) => u.some(({ stations, by }) => arrival <= by && !stations.some((s) => visited[s])))) return;
    if (!needs.every((n) => meets(steps, n))) return;
    const c: Connection = { steps: [...steps], departure: leave, arrival, changes: rides - 1, slack };
    const k = keyOf(c);
    if (every) found.push(c);
    else if (!found.some((f) => beats(keyOf(f), k))) found = [...found.filter((f) => !beats(k, keyOf(f))), c];
  };
  // Pruning only, so none of it changes what is found: nothing found yet beats
  // whatever the partial connection can become, and no need tied to a time
  // already past is left unmet. Both by the bounds of `reach`, which let every
  // train still to board leave up to missedBy before the connection gets there.
  // Stryker disable all: pruning only, the same found without it.
  // Every train boarded is left at a station not visited yet: no more of them than such stations.
  const places = new Set([limits.origin, ...runs.flatMap((r) => r.calls.map((c) => c.station))]);
  for (const [a, to] of walks) for (const b of [a, ...to.keys()]) places.add(b);
  const bounds =
    every && needs.length === 0
      ? null
      : reach(departures, walks, limits.destination, limits.missedBy, Math.min(limits.maxChanges + 1, places.size));
  const hopeless = (station: number, ready: number): boolean => {
    // Before the first train the times are not fixed yet: a walk from the origin starts when it leaves.
    if (rides === 0 || !bounds) return false;
    const last = steps[steps.length - 1];
    const behind = last?.kind === "ride" ? item(last.calls, last.calls.length - 2).station : -1;
    const seen = steps.reduce((n, s) => n + (s.kind === "walk" ? 1 : s.calls.length - 1), 1);
    const [arrival, step] = bounds(station, ready, behind, Math.min(limits.maxChanges + 1 - rides, places.size - seen));
    const bound: Key = {
      dep: leave,
      arr: arrival,
      changes: rides - 1,
      slack: Math.min(slack ?? ENOUGH_BUFFER, ENOUGH_BUFFER),
    };
    if (!every && found.some((f) => beats(keyOf(f), bound))) return true;
    return needs.some((n) => (needTime(n) ?? Infinity) < step && !meets(steps, n));
  };
  // Stryker restore all

  const explore = (station: number, ready: number, deadline: number | null, walked: boolean, lead: number): void => {
    // Stryker disable next-line EqualityOperator: one step more or less under the cap changes nothing that matters.
    if (++nodes > maxNodes) {
      complete = false;
      return;
    }
    if (station === limits.destination) {
      record(ready);
      return;
    }
    // Stryker disable next-line ConditionalExpression: pruning only.
    if (hopeless(station, ready)) return;
    // From the origin a walk to a nearby station first; later one after a train.
    if (!walked)
      for (const [to, d] of walks.get(station) ?? []) {
        if (visited[to]) continue;
        let start: number;
        if (deadline === null) {
          if (to === limits.destination) continue;
          start = limits.from;
        } else {
          start = ready;
          if (start + d > deadline) continue;
        }
        const end = start + d;
        steps.push({ kind: "walk", from: station, to, start, end });
        // The origin, where a train calling there too is boarded instead, and where the walk leads.
        const marks: Presence[] =
          deadline === null ? [{ station, arrival: null, since: limits.from, slackOut: null, starts: 0 }] : [];
        marks.push({ station: to, arrival: end, since: end, slackOut: null, starts: deadline === null ? d : null });
        presence.push(...marks);
        visited[to] = presence.length;
        // The walk's length counts against the departure only before the first train.
        explore(to, end, deadline, true, d);
        presence.length -= marks.length;
        steps.pop();
        visited[to] = 0;
      }
    if (rides > limits.maxChanges) return;
    for (const [run, k, dep] of departures.get(station) ?? []) {
      if (used.has(run.trip)) continue;
      let limit: number;
      if (deadline === null) {
        const out = dep - lead;
        if (out < limits.from) continue;
        if (out > limits.until) break;
        limit = out + limits.maxDuration;
      } else {
        // Stryker disable next-line ConditionalExpression,EqualityOperator: leaving after the deadline arrives after it too.
        if (dep > deadline) break;
        if (dep < ready - limits.missedBy) continue;
        limit = deadline;
      }
      const all = tripCalls.get(run.trip) ?? [];
      const before = all.slice(0, all.indexOf(item(run.calls, k)));
      let unless: Unless[] = [];
      if (!limits.keepDetours) {
        const u = detourUnless(presence, before, dep, limit, limits, reached);
        if (u === null) continue;
        unless = u;
      }
      deferred.push(unless);
      const saved = { leave, slack };
      const here = presence[presence.length - 1];
      const first = rides === 0;
      // The change made here, for detour checks further on; at the origin
      // (or where a walk from it led), the first train's departure.
      if (first || !here) presence.push({ station, arrival: null, since: dep, slackOut: null, starts: lead });
      else here.slackOut = here.arrival === null ? null : dep - here.arrival;
      if (first) {
        leave = dep - lead;
        // A walk to the first train starts when the connection leaves.
        const walk = steps[0];
        if (walk?.kind === "walk") steps[0] = { ...walk, start: leave, end: leave + walk.end - walk.start };
      } else slack = Math.min(slack ?? Infinity, dep - ready);
      used.add(run.trip);
      rides++;
      // The stop each station passed leads to.
      const stop = presence.length;
      const passed: number[] = [],
        through: number[] = [];
      for (let j = k + 1; j < run.calls.length; j++) {
        const c = item(run.calls, j);
        // Arriving before departing means merged data disagrees: no riding on.
        if (visited[c.station] || c.arr === null || c.arr > limit || c.arr < dep) break;
        visited[c.station] = stop + 1;
        passed.push(c.station);
        // Passed without stopping: ridden through, not left.
        if (run.skips.has(j)) {
          through.push(j - k);
          continue;
        }
        // A station the server's search didn't know (stops completed after it) is never left at.
        const lb = limits.lowerBound;
        if (lb && !((lb[c.station] ?? Infinity) < (lb[station] ?? -Infinity))) continue;
        steps.push({ kind: "ride", trip: run.trip, calls: run.calls.slice(k, j + 1), passes: [...through] });
        presence.push({ station: c.station, arrival: c.arr, since: c.arr, slackOut: null, starts: null });
        explore(c.station, c.arr, limit, false, 0);
        presence.pop();
        steps.pop();
      }
      for (const s of passed) visited[s] = 0;
      if (first || !here) presence.pop();
      else here.slackOut = null;
      deferred.pop();
      rides--;
      used.delete(run.trip);
      ({ leave, slack } = saved);
    }
  };

  visited[limits.origin] = 1;
  explore(limits.origin, limits.from, null, false, 0);
  found.sort((a, b) => b.departure - a.departure || a.arrival - b.arrival);
  return { found, complete };
}
