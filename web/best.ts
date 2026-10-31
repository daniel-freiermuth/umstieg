// The best connections passing picked lines and stations, found in the
// browser over the drawn network. Every connection within the limits lies on
// that network, and searching it by the same rules as the server (window,
// travel time, changes, changes missed by a little, walks, no station twice,
// no detours) finds exactly those connections. The detour rule needs a
// train's earlier stops, known here only where the network has them.

import { item } from "./util";

/** A stop of a train: arrival and departure in ms, either missing at its ends. */
export interface Call {
  station: number;
  arr: number | null;
  dep: number | null;
}

/** What the search needs of a piece: its kind, train and (ms, station) points. */
export interface Stretch {
  kind: "ride" | "wait" | "walk";
  trip: number | null;
  raw: readonly (readonly [number, number])[];
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
}

/** What a connection must pass: a picked stretch, stay, walk or station. */
export type Need =
  | { kind: "ride"; trip: number; from: number; at: number; to: number; until: number }
  | { kind: "stay"; station: number; at: number }
  | { kind: "walk"; from: number; to: number; at: number }
  | { kind: "station"; station: number };

export type Step =
  | { kind: "ride"; trip: number; calls: Call[] }
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
        s.kind === "walk" ? s.from === n.station || s.to === n.station : s.calls.some((c) => c.station === n.station),
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
    for (const c of s.calls.slice(1, -1)) if (c.arr !== null && c.dep !== null) out.push([c.station, c.arr, c.dep]);
    here = last.arr === null ? null : [last.station, last.arr];
  }
  return out;
}

interface Run {
  trip: number;
  calls: Call[];
}

/** A station the connection stopped at, for the detour rule (as the server's search keeps it). */
interface Presence {
  station: number;
  /** Arrival; null at the origin. */
  arrival: number | null;
  /** From when trains there count as catchable (the origin: the first departure). */
  since: number;
  /** Buffer of the change made here, once the next train is chosen. */
  slackOut: number | null;
}

/**
 * Boarding a train leaving at `dep` where the connection stands (the last
 * presence) is a detour if the train called earlier (`earlier`) at a station
 * the connection stopped at before, catchable there comfortably and with at
 * least the buffer the detour's changes leave. At the origin any later
 * departure of the train counts.
 */
export function isDetour(
  presence: readonly Presence[],
  earlier: readonly Call[],
  dep: number,
  minTransfer: number,
): boolean {
  const here = presence[presence.length - 1];
  if (!here) return false;
  let worst = here.arrival === null ? Infinity : dep - here.arrival;
  for (let i = presence.length - 2; i >= 0; i--) {
    const p = item(presence, i);
    const caught = earlier.find((c) => c.station === p.station)?.dep ?? null;
    if (caught !== null && caught >= p.since) {
      if (p.arrival === null) return true;
      const direct = caught - p.arrival;
      if (direct >= minTransfer && direct >= Math.min(worst, p.slackOut ?? Infinity)) return true;
    }
    worst = Math.min(worst, p.slackOut ?? Infinity);
  }
  return false;
}

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
      runs.push({ trip: p.trip, calls: callsOf(p.raw) });
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
  const tripCalls = new Map<number, Call[]>();
  for (const run of runs) tripCalls.set(run.trip, [...(tripCalls.get(run.trip) ?? []), ...run.calls]);
  for (const calls of tripCalls.values()) calls.sort((a, b) => (a.dep ?? a.arr ?? 0) - (b.dep ?? b.arr ?? 0));
  // Departures per station, earliest first: (run, call, time).
  const departures = new Map<number, [Run, number, number][]>();
  for (const run of runs)
    run.calls.forEach((c, k) => {
      if (c.dep === null) return;
      const list = departures.get(c.station) ?? [];
      departures.set(c.station, list);
      list.push([run, k, c.dep]);
    });
  for (const list of departures.values()) list.sort((a, b) => a[2] - b[2]);

  const visited = new Uint8Array(stations);
  const presence: Presence[] = [];
  const used = new Set<number>();
  const steps: Step[] = [];
  let found: Connection[] = [];
  let nodes = 0;
  let complete = true;
  let rides = 0;
  let leave = 0;
  let slack: number | null = null;

  const record = (arrival: number): void => {
    if (!needs.every((n) => meets(steps, n))) return;
    const c: Connection = { steps: [...steps], departure: leave, arrival, changes: rides - 1, slack };
    const k = keyOf(c);
    if (every) found.push(c);
    else if (!found.some((f) => beats(keyOf(f), k))) found = [...found.filter((f) => !beats(k, keyOf(f))), c];
  };
  // Pruning only, so none of it changes what is found: nothing found yet beats
  // whatever the partial connection can become, and no need tied to a time
  // already past (allowing a missed change) is left unmet.
  // Stryker disable all: pruning only, the same found without it.
  const hopeless = (ready: number): boolean => {
    const bound: Key = {
      dep: leave,
      arr: ready,
      changes: rides - 1,
      slack: Math.min(slack ?? ENOUGH_BUFFER, ENOUGH_BUFFER),
    };
    if (!every && found.some((f) => beats(keyOf(f), bound))) return true;
    return needs.some((n) => (needTime(n) ?? Infinity) < ready - limits.missedBy && !meets(steps, n));
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
    if (hopeless(ready)) return;
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
        visited[to] = 1;
        steps.push({ kind: "walk", from: station, to, start, end });
        // The origin, where a train calling there too is boarded instead, and where the walk leads.
        const marks: Presence[] =
          deadline === null ? [{ station, arrival: null, since: limits.from, slackOut: null }] : [];
        marks.push({ station: to, arrival: end, since: end, slackOut: null });
        presence.push(...marks);
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
      if (!limits.keepDetours && isDetour(presence, before, dep, limits.minTransfer)) continue;
      const saved = { leave, slack };
      const here = presence[presence.length - 1];
      const first = rides === 0;
      // The change made here, for detour checks further on; at the origin
      // (or where a walk from it led), the first train's departure.
      if (first || !here) presence.push({ station, arrival: null, since: dep, slackOut: null });
      else here.slackOut = here.arrival === null ? null : dep - here.arrival;
      if (first) {
        leave = dep - lead;
        // A walk to the first train starts when the connection leaves.
        const walk = steps[0];
        if (walk?.kind === "walk") steps[0] = { ...walk, start: leave, end: leave + walk.end - walk.start };
      } else slack = Math.min(slack ?? Infinity, dep - ready);
      used.add(run.trip);
      rides++;
      const passed: number[] = [];
      for (let j = k + 1; j < run.calls.length; j++) {
        const c = item(run.calls, j);
        // Arriving before departing means merged data disagrees: no riding on.
        if (visited[c.station] || c.arr === null || c.arr > limit || c.arr < dep) break;
        visited[c.station] = 1;
        passed.push(c.station);
        steps.push({ kind: "ride", trip: run.trip, calls: run.calls.slice(k, j + 1) });
        presence.push({ station: c.station, arrival: c.arr, since: c.arr, slackOut: null });
        explore(c.station, c.arr, limit, false, 0);
        presence.pop();
        steps.pop();
      }
      for (const s of passed) visited[s] = 0;
      if (first || !here) presence.pop();
      else here.slackOut = null;
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
