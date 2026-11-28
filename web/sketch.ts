// What a running search has seen so far (`umstieg serve`), drawn behind the
// progress page: the items its status brings, decoded, and the picture they
// add up to — the trains every answer names, the stations asked about, and
// once built, the timetable on its final heights. No DOM: progress.ts draws.

import {
  DecodeError,
  type Decoder,
  type Json,
  array,
  field,
  nullable,
  number,
  object,
  oneOf,
  pair,
  string,
} from "./decode";
import { item } from "./util";

export type LatLon = [lat: number, lon: number];
export interface Place {
  name: string;
  at: LatLon;
}
/** Where the search goes, and when it may leave (Unix seconds). */
export interface Ends {
  from: Place;
  to: Place;
  depart_from: number;
  depart_until: number;
}
/** A train an asked station's answer lists: [train id, arrival, departure]. */
export type Seen = [id: string, arrival: number | null, departure: number | null];
/** A train's stop as an answer has it (Unix seconds, live where known). */
export interface Stop {
  at: LatLon;
  arrival: number | null;
  departure: number | null;
}
/** A timetable station; `height`: fastest remaining ride minutes, `null` where the goal is out of reach. */
export interface Station {
  name: string;
  at: LatLon | null;
  height: number | null;
}
/** A timetable train's stop: [station index, arrival, departure]. */
export type Call = [station: number, arrival: number | null, departure: number | null];
export interface Timetable {
  stations: Station[];
  trains: Call[][];
}

export type Item =
  | ({ kind: "ends" } & Ends)
  | { kind: "asked"; place: Place; span: [number, number] | null; seen: Seen[] }
  | { kind: "train"; id: string; stops: Stop[] }
  | ({ kind: "timetable" } & Timetable)
  | { kind: "asked_train"; trip: number };

const time = nullable(number);
const latLon: Decoder<LatLon> = pair(number, number);

/** A JSON array of exactly `n` values. */
function tuple(v: Json, path: string, n: number): readonly Json[] {
  const xs = array((x: Json) => x)(v, path);
  if (xs.length !== n) throw new DecodeError(`${path}: expected ${String(n)} values`);
  return xs;
}
/** Value `i` of a tuple, decoded. */
const at = <T>(xs: readonly Json[], i: number, decode: Decoder<T>, path: string): T =>
  decode(item(xs, i), `${path}[${String(i)}]`);

const place: Decoder<Place> = (v, path) => {
  const o = object(v, path);
  return { name: field(o, "name", string, path), at: field(o, "at", latLon, path) };
};
const seen: Decoder<Seen> = (v, path) => {
  const xs = tuple(v, path, 3);
  return [at(xs, 0, string, path), at(xs, 1, time, path), at(xs, 2, time, path)];
};
const stop: Decoder<Stop> = (v, path) => {
  const xs = tuple(v, path, 4);
  return {
    at: [at(xs, 0, number, path), at(xs, 1, number, path)],
    arrival: at(xs, 2, time, path),
    departure: at(xs, 3, time, path),
  };
};
const call: Decoder<Call> = (v, path) => {
  const xs = tuple(v, path, 3);
  return [at(xs, 0, number, path), at(xs, 1, time, path), at(xs, 2, time, path)];
};
const station: Decoder<Station> = (v, path) => {
  const o = object(v, path);
  return {
    name: field(o, "name", string, path),
    at: field(o, "at", nullable(latLon), path),
    height: field(o, "height", nullable(number), path),
  };
};

export const decodeItem: Decoder<Item> = (v, path) => {
  const o = object(v, path);
  const kind = field(o, "kind", oneOf("ends", "asked", "train", "timetable", "asked_train"), path);
  switch (kind) {
    case "ends":
      return {
        kind,
        from: field(o, "from", place, path),
        to: field(o, "to", place, path),
        depart_from: field(o, "depart_from", number, path),
        depart_until: field(o, "depart_until", number, path),
      };
    case "asked":
      return {
        kind,
        place: field(o, "place", place, path),
        span: field(o, "span", nullable(pair(number, number)), path),
        seen: field(o, "seen", array(seen), path),
      };
    case "train":
      return { kind, id: field(o, "id", string, path), stops: field(o, "stops", array(stop), path) };
    case "timetable":
      return {
        kind,
        stations: field(o, "stations", array(station), path),
        trains: field(o, "trains", array(array(call)), path),
      };
    case "asked_train":
      return { kind, trip: field(o, "trip", number, path) };
  }
};

/** What the items so far add up to. */
export interface Picture {
  ends: Ends | null;
  /** Trains by id, each a stop per position in time order; drawn until the timetable comes. */
  trains: Map<string, Stop[]>;
  timetable: Timetable | null;
}

export const emptyPicture = (): Picture => ({ ends: null, trains: new Map(), timetable: null });

/** What an item shows besides the picture: a station asked about, a timetable train asked about, the timetable come. */
export type Effect =
  { asked: Place; span: [number, number] | null } | { askedTrain: number } | { timetable: true } | null;

/** When a stop is: its arrival, else its departure. */
const when = (s: Stop): number => s.arrival ?? s.departure ?? 0;

/**
 * `add` into `stops` (a stop per position, in time order): at a known
 * position it fills in the times that stop lacks; a stop without times is
 * left out.
 */
export function addStop(stops: Stop[], add: Stop): void {
  if (add.arrival === null && add.departure === null) return;
  const same = stops.find((s) => s.at[0] === add.at[0] && s.at[1] === add.at[1]);
  if (same) {
    same.arrival ??= add.arrival;
    same.departure ??= add.departure;
    return;
  }
  const after = stops.findIndex((s) => when(s) > when(add));
  stops.splice(after < 0 ? stops.length : after, 0, { at: add.at, arrival: add.arrival, departure: add.departure });
}

/** `item` into `picture`; trains named after the timetable came are left out (it has them all). */
export function apply(picture: Picture, it: Item): Effect {
  const train = (id: string): Stop[] => {
    let stops = picture.trains.get(id);
    if (stops === undefined) picture.trains.set(id, (stops = []));
    return stops;
  };
  switch (it.kind) {
    case "ends":
      picture.ends = { from: it.from, to: it.to, depart_from: it.depart_from, depart_until: it.depart_until };
      return null;
    case "asked":
      if (picture.timetable === null) {
        for (const [id, arrival, departure] of it.seen) addStop(train(id), { at: it.place.at, arrival, departure });
      }
      return { asked: it.place, span: it.span };
    case "train":
      if (picture.timetable === null) for (const s of it.stops) addStop(train(it.id), s);
      return null;
    case "timetable":
      picture.timetable = { stations: it.stations, trains: it.trains };
      picture.trains.clear();
      return { timetable: true };
    case "asked_train":
      return { askedTrain: it.trip };
  }
}

const EARTH_KM = 6371;
const rad = (deg: number): number => (deg * Math.PI) / 180;

/** Great-circle distance in km. */
export function km(a: LatLon, b: LatLon): number {
  const h =
    Math.sin(rad(b[0] - a[0]) / 2) ** 2 +
    Math.cos(rad(a[0])) * Math.cos(rad(b[0])) * Math.sin(rad(b[1] - a[1]) / 2) ** 2;
  return 2 * EARTH_KM * Math.asin(Math.sqrt(h));
}

/**
 * Distance left along the line from start to goal, in km: the start at the
 * line's length, the goal at 0 (plot.rs's `axis` metric). The heights until
 * the timetable gives the real ones.
 */
export function axis(at: LatLon, ends: Ends): number {
  const total = km(ends.from.at, ends.to.at);
  if (total === 0) return 0;
  const a = km(at, ends.from.at);
  const b = km(at, ends.to.at);
  return total - (a * a + total * total - b * b) / (2 * total);
}

/**
 * The range of `values` without the `cut` share at either end (a train padded
 * in days later doesn't squeeze the rest into a corner), widened to `keep`.
 */
export function spread(values: readonly number[], cut: number, keep: [number, number]): [number, number] {
  if (values.length === 0) return keep;
  const sorted = values.toSorted((a, b) => a - b);
  const k = Math.floor(cut * (sorted.length - 1));
  return [Math.min(keep[0], item(sorted, k)), Math.max(keep[1], item(sorted, sorted.length - 1 - k))];
}

/** `v` in [lo, hi] as 0..1; the middle if the range is empty. */
export const unit = (v: number, [lo, hi]: [number, number]): number => (hi > lo ? (v - lo) / (hi - lo) : 0.5);

/** A stop to draw: its height (`null`: not drawn) and times. */
export type Corner = [height: number | null, arrival: number | null, departure: number | null];

/**
 * A train as drawn: (time, height) at its arrival and its departure at each
 * stop with a height, in order; fewer than two points draw nothing.
 */
export function corners(stops: readonly Corner[]): [number, number][] {
  const points: [number, number][] = [];
  for (const [height, arrival, departure] of stops) {
    if (height === null) continue;
    if (arrival !== null) points.push([arrival, height]);
    if (departure !== null && departure !== arrival) points.push([departure, height]);
  }
  return points.length < 2 ? [] : points;
}

/** The timetable station nearest `at`, less than `within` km away; `null` if none. */
export function nearest(stations: readonly Station[], at: LatLon, within: number): number | null {
  let best: number | null = null;
  let bestKm = within;
  for (const [i, s] of stations.entries()) {
    if (s.at === null) continue;
    const d = km(s.at, at);
    // Stryker disable next-line EqualityOperator: equally near stations are one place; either is right.
    if (d < bestKm) [best, bestKm] = [i, d];
  }
  return best;
}
