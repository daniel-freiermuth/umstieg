// The plot's data as serialized by src/plot.rs, decoded and linked up.

import { type Decoder, array, boolean, field, nullable, number, object, oneOf, pair, string } from "./decode";
import type { Domain, Pt } from "./geometry";
import { item } from "./util";

export interface Data {
  title: string;
  subtitle: string;
  back: string | null;
  tz: string; // the user's zone every time is shown in: "Europe/Berlin"
  metrics: Metric[];
  metric: number; // index into metrics shown first
  stations: number[]; // station ids with a row
  names: string[]; // station names by id
  trips: string[]; // "ICE 378 → Berlin Hbf" by trip id
  groups: { label: string; count: number }[];
  pieces: PieceData[];
  /** The pieces on some connection within the limits: what is drawn. */
  network: number[];
  fallbacks: { at: string; line: string; pieces: number[] }[];
  chains: { group: number; header: string; pieces: number[] }[];
  /** The connections' details, if in the page; otherwise fetched by connection. */
  details: DetailData[] | null;
  /** Where to fetch details (`?ids=`), when not in the page. */
  details_url: string | null;
}
/** What a connection's selection shows: its route and fallbacks (index, arrival). */
export interface DetailData {
  route: string;
  fallbacks: [number, string][];
}
export interface Metric {
  id: string;
  unit: string;
  description: string;
  heights: (number | null)[];
}
interface PieceData {
  kind: Kind;
  info: string;
  warn: boolean;
  trip: number | null;
  pts: [number, number][];
}
const kinds = ["ride", "wait", "walk"] as const;
export type Kind = (typeof kinds)[number];

const decodeMetric: Decoder<Metric> = (v, path) => {
  const o = object(v, path);
  return {
    id: field(o, "id", string, path),
    unit: field(o, "unit", string, path),
    description: field(o, "description", string, path),
    heights: field(o, "heights", array(nullable(number)), path),
  };
};
const decodePiece: Decoder<PieceData> = (v, path) => {
  const o = object(v, path);
  return {
    kind: field(o, "kind", oneOf(...kinds), path),
    info: field(o, "info", string, path),
    warn: field(o, "warn", boolean, path),
    trip: field(o, "trip", nullable(number), path),
    pts: field(o, "pts", array(pair(number, number)), path),
  };
};
const decodeDetail: Decoder<DetailData> = (v, path) => {
  const o = object(v, path);
  return {
    route: field(o, "route", string, path),
    fallbacks: field(o, "fallbacks", array(pair(number, string)), path),
  };
};
/** Details as `umstieg serve` hands them out (`/job/{id}/chains`). */
export const decodeDetails: Decoder<DetailData[]> = array(decodeDetail);

export const decodeData: Decoder<Data> = (v, path) => {
  const o = object(v, path);
  return {
    title: field(o, "title", string, path),
    subtitle: field(o, "subtitle", string, path),
    back: field(o, "back", nullable(string), path),
    tz: field(o, "tz", string, path),
    metrics: field(o, "metrics", array(decodeMetric), path),
    metric: field(o, "metric", number, path),
    stations: field(o, "stations", array(number), path),
    names: field(o, "names", array(string), path),
    trips: field(o, "trips", array(string), path),
    groups: field(
      o,
      "groups",
      array((g, p) => {
        const og = object(g, p);
        return { label: field(og, "label", string, p), count: field(og, "count", number, p) };
      }),
      path,
    ),
    pieces: field(o, "pieces", array(decodePiece), path),
    network: field(o, "network", array(number), path),
    fallbacks: field(
      o,
      "fallbacks",
      array((f, p) => {
        const of = object(f, p);
        return {
          at: field(of, "at", string, p),
          line: field(of, "line", string, p),
          pieces: field(of, "pieces", array(number), p),
        };
      }),
      path,
    ),
    chains: field(
      o,
      "chains",
      array((c, p) => {
        const oc = object(c, p);
        return {
          group: field(oc, "group", number, p),
          header: field(oc, "header", string, p),
          pieces: field(oc, "pieces", array(number), p),
        };
      }),
      path,
    ),
    details: field(o, "details", nullable(decodeDetails), path),
    details_url: field(o, "details_url", nullable(string), path),
  };
};

export interface Piece {
  kind: Kind;
  info: string;
  warn: boolean;
  trip: number | null;
  raw: [ms: number, s: number][];
  pts: Pt[]; // per the chosen metric
}
export interface Fallback {
  at: string;
  line: string;
  pieces: Piece[];
  arrive: string;
}
export interface Chain {
  group: number;
  header: string;
  pieces: number[];
}
export interface Detail {
  route: string;
  fallbacks: Fallback[];
}
export interface Plot extends Omit<Data, "pieces" | "fallbacks" | "chains" | "details"> {
  pieces: Piece[];
  fallbacks: Omit<Fallback, "arrive">[];
  chains: Chain[];
  /** Details by connection, as far as known: all if in the page. */
  details: Map<number, Detail>;
}

/**
 * Points come as (unix minutes, station); pieces and fallbacks by index.
 * Fallback pieces are the plot's pieces themselves, so heights set on those
 * apply to both. `pts` is empty until heights are applied.
 */
export function load(data: Data): Plot {
  const { pieces: rawPieces, fallbacks: rawFallbacks, chains, details: rawDetails, ...rest } = data;
  const pieces = rawPieces.map(({ kind, info, warn, trip, pts }): Piece => ({
    kind,
    info,
    warn,
    trip,
    raw: pts.map(([m, s]) => [m * 60e3, s]),
    pts: [],
  }));
  const fallbacks = rawFallbacks.map((f) => ({ at: f.at, line: f.line, pieces: f.pieces.map((i) => item(pieces, i)) }));
  const plot: Plot = { ...rest, pieces, fallbacks, chains, details: new Map() };
  addDetails(
    plot,
    (rawDetails ?? []).map((d, i) => [i, d]),
  );
  return plot;
}

/**
 * Details of connections by index, linked up: fallbacks are the plot's,
 * with the connection's arrival.
 */
export function addDetails(plot: Plot, details: [number, DetailData][]): void {
  for (const [i, { route, fallbacks }] of details)
    plot.details.set(i, {
      route,
      fallbacks: fallbacks.map(([f, arrive]) => ({ ...item(plot.fallbacks, f), arrive })),
    });
}

/** A station with a row in the plot, at its height. */
export interface Row {
  i: number;
  name: string;
  y: number;
}
/**
 * Heights per metric: piece points become (ms, height, station), stations
 * without a height in it are left out; rows are the stations with a height,
 * highest first, ties by name.
 */
export function applyHeights(plot: Plot, metric: number): Row[] {
  const heights = item(plot.metrics, metric).heights;
  for (const p of plot.pieces)
    p.pts = p.raw.flatMap(([t, s]): Pt[] => {
      const h = heights[s] ?? null;
      return h === null ? [] : [[t, h, s]];
    });
  return plot.stations
    .flatMap((s) => {
      const h = heights[s] ?? null;
      return h === null ? [] : [{ i: s, name: item(plot.names, s), y: h }];
    })
    .sort((a, b) => b.y - a.y || a.name.localeCompare(b.name));
}

/**
 * Full extent of the network (fallbacks reaching beyond are clipped): its
 * times padded by 10 minutes, heights from 0 (the goal) to the highest.
 */
export function extent(plot: Plot): Domain {
  const full: Domain = { t0: Infinity, t1: -Infinity, y0: 0, y1: 0 };
  for (const i of plot.network)
    for (const [t, v] of item(plot.pieces, i).pts) {
      full.t0 = Math.min(full.t0, t);
      full.t1 = Math.max(full.t1, t);
      full.y1 = Math.max(full.y1, v);
    }
  full.t0 -= 10 * 60e3;
  full.t1 += 10 * 60e3;
  return full;
}
