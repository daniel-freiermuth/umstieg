// Connections the page found itself (best.ts), described the way the server
// describes its listed ones: header, route line, and pieces to draw.

import type { Connection, Step } from "./best";
import type { Kind } from "./data";
import { item } from "./util";

/** A piece to draw: points as (ms, station). */
export interface Drawn {
  kind: Kind;
  info: string;
  warn: boolean;
  trip: number | null;
  raw: [number, number][];
}

/** Times as the page shows them: "09:21 CET". */
export type Clock = (ms: number) => string;

/** "3h46", "14m": as the server writes durations (never negative here). */
export function duration(ms: number): string {
  const m = Math.round(ms / 60e3);
  return m >= 60 ? `${String(Math.floor(m / 60))}h${String(m % 60).padStart(2, "0")}` : `${String(m)}m`;
}

/** "12m", "3m⚠" below the minimum change time, "missed 2m✗" for a missed change. */
export function buffer(ms: number, minTransfer: number): string {
  if (ms < 0) return `missed ${duration(-ms)}✗`;
  return `${duration(ms)}${ms < minTransfer ? "⚠" : ""}`;
}

/** A train's name without where it goes: "ICE 593" of "ICE 593 → München Hbf". */
export const trainName = (trip: string): string => trip.split(" → ")[0] ?? trip;

/** One key per way of travelling: the same trains, stops, times and walks. */
export function signature(steps: readonly Step[]): string {
  return steps
    .map((s) => {
      if (s.kind === "walk") return `w${String(s.from)}>${String(s.to)}@${String(s.start)}`;
      const a = s.calls[0],
        b = s.calls[s.calls.length - 1];
      return `${String(s.trip)}:${String(a?.station)}@${String(a?.dep)}>${String(b?.station)}@${String(b?.arr)}`;
    })
    .join(" ");
}

/** The same key for a listed connection, from its pieces (waits don't count). */
export function signatureOfPieces(pieces: readonly Drawn[]): string {
  return pieces
    .flatMap((p) => {
      const a = p.raw[0],
        b = p.raw[p.raw.length - 1];
      if (!a || !b || p.kind === "wait") return [];
      if (p.kind === "walk") return [`w${String(a[1])}>${String(b[1])}@${String(a[0])}`];
      return [`${String(p.trip)}:${String(a[1])}@${String(a[0])}>${String(b[1])}@${String(b[0])}`];
    })
    .join(" ");
}

/** "09:21 CET → 12:41 CET  3h20  1×  min 14m" */
export function header(c: Connection, minTransfer: number, clock: Clock): string {
  const min = c.slack === null ? "direct" : `min ${buffer(c.slack, minTransfer)}`;
  return `${clock(c.departure)} → ${clock(c.arrival)}  ${duration(c.arrival - c.departure)}  ${String(c.changes)}×  ${min}`;
}

/**
 * Trains, change stations and buffers on one line, from a walk to the first
 * train to a walk to the destination: "09:21 CET ICE 1067 → 11:03 CET
 * Stuttgart Hbf ‖14m‖ 11:17 CET ICE 593 → 12:41 CET Augsburg Hbf".
 */
export function route(
  c: Connection,
  names: readonly string[],
  trips: readonly string[],
  minTransfer: number,
  clock: Clock,
): string {
  const name = (s: number): string => item(names, s);
  const rides = c.steps.flatMap((s, i) => (s.kind === "ride" ? [i] : []));
  const [first, final] = [rides[0] ?? 0, rides[rides.length - 1] ?? 0];
  const walks = (steps: readonly Step[]): string[] =>
    // Stryker disable next-line ArrayDeclaration: only walks come before the first train and after the last.
    steps.flatMap((s) => (s.kind === "walk" ? [`walk ${duration(s.end - s.start)} to ${name(s.to)}`] : []));
  // Walks before the first train and after the last are listed; between trains they belong to the change.
  const parts = walks(c.steps.slice(0, first));
  // Where the last train arrived, when, and the walk since (counted in the next change).
  let last: [station: number, at: number] | null = null;
  let walked = 0;
  for (const s of c.steps) {
    if (s.kind === "walk") {
      walked += s.end - s.start;
      continue;
    }
    const a = item(s.calls, 0),
      b = item(s.calls, s.calls.length - 1);
    if (a.dep === null || b.arr === null) continue;
    let from = "";
    if (last !== null) {
      const walk = walked > 0 ? ` walk ${duration(walked)}` : "";
      parts.push(`‖${buffer(a.dep - last[1] - walked, minTransfer)}${walk}‖`);
      if (name(a.station) !== name(last[0])) from = ` ${name(a.station)}`;
    }
    parts.push(`${clock(a.dep)}${from} ${trainName(item(trips, s.trip))} → ${clock(b.arr)} ${name(b.station)}`);
    last = [b.station, b.arr];
    walked = 0;
  }
  parts.push(...walks(c.steps.slice(final + 1)));
  return parts.join(" ");
}

/**
 * Pieces drawing `c` as the server draws a connection: rides from boarding
 * to alighting, waits at changes (a missed one from departure to arrival,
 * red like other too short ones), walks.
 */
export function piecesOf(
  c: Connection,
  names: readonly string[],
  trips: readonly string[],
  minTransfer: number,
  clock: Clock,
): Drawn[] {
  const name = (s: number): string => item(names, s);
  const out: Drawn[] = [];
  // Where and since when the connection is, once a train has been taken.
  let here: [station: number, since: number] | null = null;
  for (const s of c.steps) {
    if (s.kind === "walk") {
      const start: number = here === null ? s.start : here[1];
      const end = start + s.end - s.start;
      out.push({
        kind: "walk",
        info: `walk ${duration(s.end - s.start)} to ${name(s.to)}`,
        warn: false,
        trip: null,
        raw: [
          [start, s.from],
          [end, s.to],
        ],
      });
      if (here !== null) here = [s.to, end];
      continue;
    }
    const a = s.calls[0],
      b = s.calls[s.calls.length - 1];
    if (!a || !b || a.dep === null || b.arr === null) continue;
    if (here !== null) {
      const wait = a.dep - here[1];
      out.push({
        kind: "wait",
        info: `${buffer(wait, minTransfer)} at ${name(a.station)}`,
        warn: wait < minTransfer,
        trip: null,
        raw: [
          [Math.min(here[1], a.dep), a.station],
          [Math.max(here[1], a.dep), a.station],
        ],
      });
    }
    const raw: [number, number][] = [];
    s.calls.forEach((call, k) => {
      if (k > 0 && call.arr !== null) raw.push([call.arr, call.station]);
      if (k < s.calls.length - 1 && call.dep !== null) raw.push([call.dep, call.station]);
    });
    out.push({
      kind: "ride",
      info: `${item(trips, s.trip)} · ${clock(a.dep)} ${name(a.station)} → ${clock(b.arr)} ${name(b.station)}`,
      warn: false,
      trip: s.trip,
      raw,
    });
    here = [b.station, b.arr];
  }
  return out;
}
