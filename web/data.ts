// The plot's data as serialized by src/plot.rs, decoded and linked up.

import { type Decoder, array, boolean, field, nullable, number, object, oneOf, pair, string } from "./decode";
import type { Domain, Pt } from "./geometry";
import { item } from "./util";

export interface Data {
  title: string;
  subtitle: string;
  back: string | null;
  tz: string;
  metrics: Metric[];
  metric: number; // index into metrics shown first
  stations: number[]; // station ids with a row
  names: string[]; // station names by id
  trips: string[]; // "ICE 378 → Berlin Hbf" by trip id
  groups: { label: string; count: number }[];
  pieces: PieceData[];
  fallbacks: { at: string; line: string; pieces: number[] }[];
  chains: { group: number; header: string; route: string; pieces: number[]; fallbacks: [number, string][] }[];
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
          route: field(oc, "route", string, p),
          pieces: field(oc, "pieces", array(number), p),
          fallbacks: field(oc, "fallbacks", array(pair(number, string)), p),
        };
      }),
      path,
    ),
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
  route: string;
  pieces: number[];
  fallbacks: Fallback[];
}
export interface Plot extends Omit<Data, "pieces" | "fallbacks" | "chains"> {
  pieces: Piece[];
  chains: Chain[];
}

/**
 * Points come as (unix minutes, station); pieces and fallbacks by index.
 * Fallback pieces are the plot's pieces themselves, so heights set on those
 * apply to both. `pts` is empty until heights are applied.
 */
export function load(data: Data): Plot {
  const { pieces: rawPieces, fallbacks: rawFallbacks, chains: rawChains, ...rest } = data;
  const pieces = rawPieces.map(({ kind, info, warn, trip, pts }): Piece => ({
    kind,
    info,
    warn,
    trip,
    raw: pts.map(([m, s]) => [m * 60e3, s]),
    pts: [],
  }));
  const fallbacks = rawFallbacks.map((f) => ({ at: f.at, line: f.line, pieces: f.pieces.map((i) => item(pieces, i)) }));
  const chains = rawChains.map(({ group, header, route, pieces: ps, fallbacks: fs }): Chain => ({
    group,
    header,
    route,
    pieces: ps,
    fallbacks: fs.map(([i, arrive]) => ({ ...item(fallbacks, i), arrive })),
  }));
  return { ...rest, pieces, chains };
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
 * Full extent of the connections (fallbacks reaching beyond are clipped):
 * their times padded by 10 minutes, heights from 0 (the goal) to the highest.
 */
export function extent(plot: Plot): Domain {
  const full: Domain = { t0: Infinity, t1: -Infinity, y0: 0, y1: 0 };
  for (const c of plot.chains)
    for (const i of c.pieces)
      for (const [t, v] of item(plot.pieces, i).pts) {
        full.t0 = Math.min(full.t0, t);
        full.t1 = Math.max(full.t1, t);
        full.y1 = Math.max(full.y1, v);
      }
  full.t0 -= 10 * 60e3;
  full.t1 += 10 * 60e3;
  return full;
}
