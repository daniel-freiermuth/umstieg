import {
  type Decoder,
  array,
  boolean,
  field,
  nullable,
  number,
  object,
  oneOf,
  pair,
  parseJson,
  string,
} from "./decode";
import { byId } from "./dom";

// The data as serialized by src/plot.rs.
interface Data {
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
interface Metric {
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
type Kind = (typeof kinds)[number];

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
const decodeData: Decoder<Data> = (v, path) => {
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
// A point in the plot: (ms, height, station).
type Pt = [t: number, y: number, s: number];
// Anything drawable as a polyline: (ms, height, …).
type XY = readonly [number, number, ...number[]];
type Interval = [from: number, to: number];
// A reachable stretch of a piece: [piece, from, to].
type Segment = [piece: number, from: number, to: number];
interface Piece {
  kind: Kind;
  info: string;
  warn: boolean;
  trip: number | null;
  raw: [ms: number, s: number][];
  pts: Pt[]; // per the chosen metric
}
interface Fallback {
  at: string;
  line: string;
  pieces: Piece[];
  arrive: string;
}
interface Chain {
  group: number;
  header: string;
  route: string;
  pieces: number[];
  fallbacks: Fallback[];
}
interface Plot extends Omit<Data, "pieces" | "fallbacks" | "chains"> {
  pieces: Piece[];
  chains: Chain[];
}
interface Reach {
  key: string;
  with?: Set<number>;
  segments: Segment[];
}
// What the mouse is over: a line, a station, a fallback.
interface Hover {
  info: string;
  chains: number[];
  pts: XY[];
  trip?: number;
  reach?: Reach;
  fallbacks?: Fallback[];
}
interface Target extends Hover {
  id: string;
}
interface Picked {
  id: string;
  info: string;
  chains: number[];
  reach: Reach | null;
}
interface Candidates {
  info: string;
  chains: number[];
  reach: Reach | null;
}
interface Row {
  i: number;
  name: string;
  y: number;
}
interface Domain {
  t0: number;
  t1: number;
  y0: number;
  y1: number;
}
interface Margins {
  l: number;
  r: number;
  t: number;
  b: number;
}
interface Geom {
  W: number;
  H: number;
  m: Margins;
}
// A stretch of time at a station within a connection: piece position j, for
// standing trains point k.
interface Span {
  from: number;
  to: number;
  chain: number;
  j: number;
  trip?: number;
  k?: number;
}
interface Edge {
  trip: number;
  pts: [Pt, Pt];
  info: string;
  chains: Set<number>;
}
interface Wait {
  y: number;
  spans: Span[];
  runs: Interval[];
}
interface HitTarget {
  pts: XY[];
  at: (time: number) => Target;
}
interface FbTarget {
  at: (time: number) => Hover;
}
interface Model {
  edges: Edge[];
  waits: Wait[];
  walks: number[];
  warns: number[];
  targets: HitTarget[];
  stationTarget: (s: number) => Target;
}
interface View {
  svg: SVGSVGElement;
  x: (t: number) => number;
  y: (v: number) => number;
  pts: (p: { pts: readonly XY[] }) => string;
  overlay: SVGGElement;
  hoverLayer: SVGGElement;
  fbLayer: SVGGElement;
  labels: SVGGElement;
  overlayKey: string | null;
  fbKey: number | null | undefined;
  fbTargets: FbTarget[];
}

// Element i, which the data guarantees to exist.
function item<T>(xs: readonly T[], i: number): T {
  const x = xs[i];
  if (x === undefined) throw new Error(`index ${String(i)} out of range`);
  return x;
}
// The entry of k, created by make if missing.
function entry<K, V>(m: Map<K, V>, k: K, make: () => V): V {
  let v = m.get(k);
  if (v === undefined) {
    v = make();
    m.set(k, v);
  }
  return v;
}

// Points come as (unix minutes, station); pieces and fallbacks by index.
function load(data: Data): Plot {
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
const D = load(decodeData(parseJson(byId("data", HTMLScriptElement).text), "data"));

const NS = "http://www.w3.org/2000/svg";
const plotEl = byId("plot", HTMLDivElement);
const tip = byId("tip", HTMLDivElement);
const details = byId("details", HTMLDivElement);
const panelTitle = byId("panel-title", HTMLHeadingElement);
const picksEl = byId("picks", HTMLDivElement);
const timeFmt = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
  timeZone: D.tz,
});
// One colour per train, so a train can be followed across all connections.
const trainColor = (t: number): string => `hsl(${String((t * 137.508) % 360)}, 70%, 45%)`;
const HINT =
  "Hover a line or station name to see what is reachable through it. Click it to pick it: the connections " +
  "passing it are listed (one alone is selected). Click more to narrow down, a pick again to remove it.";

// Interaction state. Selection only changes on click; hovering only previews.
const hidden = new Set<number>(); // hidden groups
let pinned: number | null = null; // selected connection
let picks: Picked[] = []; // clicked lines/stations narrowing the list
let candidates: Candidates | null = null; // connections passing every pick
let hoverTarget: Hover | null = null; // line under the mouse
let preview: number | null = null; // connection hovered in the candidate list
let softGroup: number | null = null; // group hovered in the group list

byId("title", HTMLHeadingElement).textContent = D.title;
byId("subtitle", HTMLParagraphElement).textContent = D.subtitle;
if (D.back) Object.assign(byId("new-search", HTMLAnchorElement), { href: D.back, hidden: false });
const legend = byId("legend", HTMLDivElement);
function writeLegend(): void {
  const metric = item(D.metrics, metricIndex);
  legend.innerHTML =
    `x: time (${D.tz}). y: ${metric.description} (${metric.unit}), goal at the bottom.<br>` +
    `Coloured: riding, one colour per train · thin grey: waiting (red: below minimum transfer, or missed by live data ✗) · dotted: walking.<br>` +
    `Parallel lines: same route later. Steeper: faster. Dashed: fallbacks of the selection (orange dots: waiting).<br>` +
    `<kbd>↑</kbd> <kbd>↓</kbd> step through listed connections, <kbd>Esc</kbd> clears the selection.<br>` +
    `Wheel: zoom (<kbd>Shift</kbd> time only, <kbd>Alt</kbd> height only) · drag: pan · double-click: reset.`;
}

function el<K extends keyof SVGElementTagNameMap>(
  name: K,
  attrs: Record<string, string | number>,
  parent?: Element,
): SVGElementTagNameMap[K] {
  const e = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, String(v));
  if (parent) parent.appendChild(e);
  return e;
}
const ESCAPES: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" };
function esc(s: string): string {
  return s.replace(/[&<>"]/g, (ch) => ESCAPES[ch] ?? ch);
}

let metricIndex = D.metric;
let rows: Row[] = []; // stations with a row, top to bottom
const full: Domain = { t0: Infinity, t1: -Infinity, y0: 0, y1: 0 };
let dom: Domain = { ...full }; // visible part: zoomed and panned
let model: Model | null = null; // shared stretches of the visible connections
let view: View | null = null; // per-render drawing context
let geom: Geom | null = null; // plot area of the last render

// Height of station s per the chosen metric, null if it has none.
function heightOf(s: number): number | null {
  return item(D.metrics, metricIndex).heights[s] ?? null;
}
// Heights per the chosen metric: points become (ms, height, station); stations
// without a height in it are left out. Zoom resets to the full extent.
function applyMetric(k: number): void {
  metricIndex = k;
  for (const p of D.pieces)
    p.pts = p.raw.flatMap(([t, s]): Pt[] => {
      const h = heightOf(s);
      return h === null ? [] : [[t, h, s]];
    });
  rows = D.stations
    .flatMap((s): Row[] => {
      const h = heightOf(s);
      return h === null ? [] : [{ i: s, name: item(D.names, s), y: h }];
    })
    .sort((a, b) => b.y - a.y || a.name.localeCompare(b.name));
  // Full extent of the connections; fallbacks reaching beyond are clipped.
  Object.assign(full, { t0: Infinity, t1: -Infinity, y0: 0, y1: 0 });
  for (const c of D.chains)
    for (const i of c.pieces)
      for (const [t, v] of item(D.pieces, i).pts) {
        full.t0 = Math.min(full.t0, t);
        full.t1 = Math.max(full.t1, t);
        full.y1 = Math.max(full.y1, v);
      }
  full.t0 -= 10 * 60e3;
  full.t1 += 10 * 60e3;
  dom = { ...full };
  writeLegend();
}
const metricSelect = byId("metric", HTMLSelectElement);
D.metrics.forEach((m, k) => {
  const o = document.createElement("option");
  o.value = String(k);
  o.textContent = m.id;
  o.title = `${m.description} (${m.unit})`;
  metricSelect.appendChild(o);
});
metricSelect.value = String(D.metric);
metricSelect.addEventListener("change", () => {
  applyMetric(+metricSelect.value);
  buildModel();
  render();
});
applyMetric(D.metric);

// Spans ({ from, to }) merged into continuous runs [from, to].
function runsOf(spans: readonly { from: number; to: number }[]): Interval[] {
  const runs: Interval[] = [];
  for (const w of [...spans].sort((a, b) => a.from - b.from)) {
    const last = runs[runs.length - 1];
    if (last && w.from <= last[1]) last[1] = Math.max(last[1], w.to);
    else runs.push([w.from, w.to]);
  }
  return runs;
}
// The spans of a run covering the hovered time. The highlight is the whole
// run: highlighting only the covering spans made one line look like several
// waits (Mannheim Hbf until 08:03 before 06:58, until 10:03 after).
function stayAt<S extends { from: number; to: number }>(
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
// Reachability through station-times: for each (spans at a station, t, back,
// fwd), every connection there arriving by t, up to the station (back), and
// every one leaving from t on, from the station (fwd). Later departures count
// too: waiting longer is always possible.
// Per connection only the earliest cut forward and the latest backward
// matter. Positions are (piece position, point): a wait at piece j belongs to
// both halves; a train standing there is cut at the stop (points k-1, k).
// The stay at the cut is clipped to the reachable time: from t on forward, up
// to t backward, so waits don't light up before anything reachable arrives.
// Returns per piece the reachable stretches as time intervals [piece, from,
// to] (pieces run forward in time), merged, so highlights of several picks
// can be intersected exactly.
type Cut = [j: number, k: number, t: number];
function reachSegments(
  items: readonly { spans: readonly Span[]; t: number; back: boolean; fwd: boolean }[],
): Segment[] {
  const from = new Map<number, Cut>(),
    upTo = new Map<number, Cut>();
  const before = (a: Cut, b: Cut): boolean => a[0] < b[0] || (a[0] === b[0] && a[1] < b[1]);
  for (const { spans, t, back, fwd } of items)
    for (const w of spans) {
      if (fwd && w.to >= t) {
        const p: Cut = w.k === undefined ? [w.j, 0, t] : [w.j, w.k - 1, t],
          q = from.get(w.chain);
        if (!q || before(p, q)) from.set(w.chain, p);
      }
      if (back && w.from <= t) {
        const n = item(D.pieces, item(item(D.chains, w.chain).pieces, w.j)).pts.length - 1;
        const p: Cut = w.k === undefined ? [w.j, n, t] : [w.j, w.k, t],
          q = upTo.get(w.chain);
        if (!q || before(q, p)) upTo.set(w.chain, p);
      }
    }
  const raw = new Map<number, Interval[]>();
  // Points k0..k1 of a piece; a flat end (wait, standing) is clipped to the cut time.
  const add = (pi: number, k0: number, k1: number, c0: number | null = null, c1: number | null = null): void => {
    const s = item(D.pieces, pi).pts;
    if (!s.length) return;
    let a = item(s, k0)[0],
      b = item(s, k1)[0];
    if (c0 !== null && k0 < k1 && item(s, k0)[2] === item(s, k0 + 1)[2])
      a = Math.min(Math.max(a, c0), item(s, k0 + 1)[0]);
    if (c1 !== null && k1 > k0 && item(s, k1)[2] === item(s, k1 - 1)[2])
      b = Math.max(Math.min(b, c1), item(s, k1 - 1)[0]);
    entry(raw, pi, () => []).push([a, b]);
  };
  const last = (pi: number): number => item(D.pieces, pi).pts.length - 1;
  for (const [chain, [j, k0, t]] of from) {
    const ps = item(D.chains, chain).pieces;
    add(item(ps, j), k0, last(item(ps, j)), t, null);
    for (let q = j + 1; q < ps.length; q++) add(item(ps, q), 0, last(item(ps, q)));
  }
  for (const [chain, [j, k1, t]] of upTo) {
    const ps = item(D.chains, chain).pieces;
    for (let q = 0; q < j; q++) add(item(ps, q), 0, last(item(ps, q)));
    add(item(ps, j), 0, k1, null, t);
  }
  const out: Segment[] = [];
  for (const [pi, list] of raw) for (const [a, b] of mergeIntervals(list)) out.push([pi, a, b]);
  return out;
}
function mergeIntervals(list: readonly Interval[]): Interval[] {
  const runs: Interval[] = [];
  for (const [a, b] of [...list].sort((x, y) => x[0] - y[0])) {
    const last = runs[runs.length - 1];
    if (last && a <= last[1]) last[1] = Math.max(last[1], b);
    else runs.push([a, b]);
  }
  return runs;
}
// Stretches reachable through all of several highlights: per piece, the
// intersection of their time intervals.
function intersectReach(segmentLists: readonly (readonly Segment[])[]): Segment[] {
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

// Whether a known time v is at most (at least) t; unknown is neither.
const atMost = (v: number | undefined, t: number): boolean => v !== undefined && v <= t;
const atLeast = (v: number | undefined, t: number): boolean => v !== undefined && v >= t;

// The smallest shared units, each drawn once and used as hover target, so
// every connection passing through counts, wherever it boards or leaves:
//   rides: one per train and stop-to-stop stretch,
//   stays: per station and continuous stay, waiting or in a standing train,
//     resolved by the time under the mouse,
//   walks: per walk.
// Depends only on which groups are hidden, not on zoom.
function buildModel(): void {
  const edges = new Map<string, Edge>(),
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
  D.chains.forEach((c, i) => {
    if (hidden.has(c.group)) return;
    c.pieces.forEach((pi, j) => {
      const p = item(D.pieces, pi);
      if (p.kind === "ride" && p.trip !== null) {
        const trip = p.trip;
        for (let k = 1; k < p.pts.length; k++) {
          const a = item(p.pts, k - 1),
            b = item(p.pts, k);
          const key = `${String(trip)}:${String(a[0])}:${String(a[2])}:${String(b[0])}:${String(b[2])}`;
          entry(edges, key, () => {
            const info = `${item(D.trips, trip)} · ${timeFmt.format(a[0])} ${item(D.names, a[2])} → ${timeFmt.format(b[0])} ${item(D.names, b[2])}`;
            return { trip, pts: [a, b], info, chains: new Set() };
          }).chains.add(i);
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
      } else if (p.kind === "walk") {
        entry(walks, pi, () => new Set()).add(i);
      }
    });
  });
  // Overlapping waits at a station merge into one line per continuous stay.
  for (const w of waits.values()) w.runs = runsOf(w.spans);
  // Hover targets, looked up by index from a single set of event listeners.
  const targets: HitTarget[] = [];
  // Reachability is transitive over everything plotted: stop-to-stop rides
  // (also standing at a stop) and walks, so changing between two trains that
  // no listed connection changes between counts too (alight at Kassel, take
  // the next train through there).
  const conns = [...edges.values()].map((e) => ({
    trip: e.trip,
    from: e.pts[0][2],
    dep: e.pts[0][0],
    to: e.pts[1][2],
    arr: e.pts[1][0],
  }));
  const byDep = [...conns].sort((a, b) => a.dep - b.dep),
    byArr = [...conns].sort((a, b) => b.arr - a.arr);
  const foot = new Map<number, Map<number, number>>(),
    footBack = new Map<number, Map<number, number>>();
  for (const pi of walks.keys()) {
    const p = item(D.pieces, pi);
    const a = p.pts[0],
      b = p.pts[p.pts.length - 1];
    if (p.pts.length < 2 || !a || !b) continue;
    entry(foot, a[2], () => new Map()).set(b[2], b[0] - a[0]);
    entry(footBack, b[2], () => new Map()).set(a[2], b[0] - a[0]);
  }
  // src: { at: [[station, time]], back: [[station, time]], ride: { trip, from,
  // until } }: being at stations at times (back: only arrived there by then),
  // or on `trip` (riding on from departure `from`, having ridden up to arrival
  // `until`). Earliest arrival forward, latest departure backward.
  interface Source {
    at: [number, number][];
    back?: [number, number][];
    fwd?: [number, number][];
    ride?: { trip: number; from: number; until: number };
  }
  const closure = (src: Source): Segment[] => {
    const ea = new Map<number, number>(),
      ld = new Map<number, number>();
    const early = (x: number, t: number): void => {
      if (atMost(ea.get(x), t)) return;
      ea.set(x, t);
      for (const [y, d] of foot.get(x) ?? []) if (!atMost(ea.get(y), t + d)) ea.set(y, t + d);
    };
    const late = (x: number, t: number): void => {
      if (atLeast(ld.get(x), t)) return;
      ld.set(x, t);
      for (const [y, d] of footBack.get(x) ?? []) if (!atLeast(ld.get(y), t - d)) ld.set(y, t - d);
    };
    for (const [s, t] of src.at) {
      early(s, t);
      late(s, t);
    }
    for (const [s, t] of src.back ?? []) late(s, t);
    for (const [s, t] of src.fwd ?? []) early(s, t);
    const boarded = new Map<number, number>(src.ride ? [[src.ride.trip, src.ride.from]] : []);
    for (const c of byDep) {
      const since = boarded.get(c.trip);
      if ((since !== undefined && c.dep >= since) || atMost(ea.get(c.from), c.dep)) {
        if (since === undefined) boarded.set(c.trip, c.dep);
        early(c.to, c.arr);
      }
    }
    const alighted = new Map<number, number>(src.ride ? [[src.ride.trip, src.ride.until]] : []);
    for (const c of byArr) {
      const until = alighted.get(c.trip);
      if ((until !== undefined && c.arr <= until) || atLeast(ld.get(c.to), c.arr)) {
        if (until === undefined) alighted.set(c.trip, c.arr);
        late(c.from, c.dep);
      }
    }
    const items: { spans: Span[]; t: number; back: boolean; fwd: boolean }[] = [];
    for (const [x, t] of ea) {
      const st = stays.get(x);
      if (st) items.push({ spans: st.spans, t, back: false, fwd: true });
    }
    for (const [x, t] of ld) {
      const st = stays.get(x);
      if (st) items.push({ spans: st.spans, t, back: true, fwd: false });
    }
    return reachSegments(items);
  };
  for (const e of edges.values()) {
    if (e.pts[0][2] === e.pts[1][2]) continue;
    // On this train between two stops: reachable is what could lead onto it,
    // what it leads to, and the connections riding it.
    const [a, b] = e.pts;
    const id = `ride:${String(e.trip)}:${String(a[0])}:${String(b[0])}`;
    let t: Target | null = null;
    targets.push({
      pts: e.pts,
      at: () => {
        t ??= {
          id,
          info: e.info,
          trip: e.trip,
          chains: [...e.chains],
          pts: e.pts,
          reach: {
            key: id,
            with: e.chains,
            segments: closure({ at: [], back: [[a[2], a[0]]], ride: { trip: e.trip, from: a[0], until: a[0] } }),
          },
        };
        return t;
      },
    });
  }
  for (const [s, { y, spans }] of stays) {
    let reach: Reach | null = null;
    for (const [from, to] of runsOf(spans))
      targets.push({
        pts: [
          [from, y, s],
          [to, y, s],
        ],
        at: (time) => {
          const { t, here, pts } = stayAt(spans, from, to, time, y, s);
          const trains = [...new Set(here.flatMap((w) => (w.trip === undefined ? [] : [w.trip])))];
          const waiting = new Set(here.filter((w) => w.trip === undefined).map((w) => w.chain)).size;
          const parts = [
            waiting ? `${String(waiting)} waiting` : null,
            ...trains.map((tr) => `standing: ${item(D.trips, tr)}`),
          ].filter((x): x is string => x !== null);
          // What is reachable only changes when t passes a span end here, so the
          // two counts identify the highlight; it is rebuilt only when they change.
          const key = `${String(s)}:${String(spans.filter((w) => w.from <= t).length)}:${String(spans.filter((w) => w.to >= t).length)}`;
          const r: Reach = reach?.key === key ? reach : { key, segments: closure({ at: [[s, t]] }) };
          reach = r;
          const trip = trains.length === 1 && !waiting ? trains[0] : undefined;
          return {
            id: `stay:${String(s)}:${String(from)}`,
            info: `at ${item(D.names, s)} · ${timeFmt.format(t)} · ${parts.join(" · ")}`,
            ...(trip === undefined ? {} : { trip }),
            chains: [...new Set(here.map((w) => w.chain))],
            reach: r,
            pts,
          };
        },
      });
  }
  for (const [pi, chains] of walks) {
    const p = item(D.pieces, pi);
    const t: Target = { id: `walk:${String(pi)}`, info: p.info, chains: [...chains], pts: p.pts };
    targets.push({ pts: t.pts, at: () => t });
  }
  // A station on the axis: every connection touching it, and what is
  // reachable through it at any time (arriving whenever, leaving whenever).
  const touching = new Map<number, Set<number>>();
  D.chains.forEach((c, i) => {
    if (hidden.has(c.group)) return;
    for (const pi of c.pieces) for (const [, , s] of item(D.pieces, pi).pts) entry(touching, s, () => new Set()).add(i);
  });
  const stationTargets = new Map<number, Target>();
  const stationTarget = (s: number): Target =>
    entry(stationTargets, s, () => {
      const chains = touching.get(s) ?? new Set<number>();
      const h = heightOf(s);
      // Only stations with a row have a label to hover.
      if (h === null) throw new Error(`station ${String(s)} has no height`);
      return {
        id: `station:${String(s)}`,
        info: `${item(D.names, s)} · any time`,
        chains: [...chains],
        pts: [
          [full.t0, h, s],
          [full.t1, h, s],
        ],
        reach: {
          key: `station:${String(s)}`,
          with: chains,
          segments: closure({ at: [], fwd: [[s, -Infinity]], back: [[s, Infinity]] }),
        },
      };
    });
  model = {
    edges: [...edges.values()],
    waits: [...waits.values()],
    walks: [...walks.keys()],
    warns: [...warns],
    targets,
    stationTarget,
  };
}

function render(): void {
  const md = model;
  if (!md) return;
  plotEl.querySelectorAll("svg").forEach((s) => {
    s.remove();
  });
  const W = plotEl.clientWidth,
    H = plotEl.clientHeight;
  const m = { l: 240, r: 16, t: 14, b: 28 };
  geom = { W, H, m };
  const x = (t: number): number => m.l + ((t - dom.t0) / (dom.t1 - dom.t0)) * (W - m.l - m.r);
  const y = (v: number): number => m.t + ((dom.y1 - v) / (dom.y1 - dom.y0 || 1)) * (H - m.t - m.b);
  const svg = el("svg", { width: W, height: H });
  plotEl.insertBefore(svg, tip);
  const clip = el("clipPath", { id: "area" }, el("defs", {}, svg));
  el("rect", { x: m.l, y: m.t - 6, width: W - m.l - m.r, height: H - m.b - m.t + 6 }, clip);

  // Station bands, alternating hue, so neighbouring stations are told apart.
  // Hue follows the station, not its position, so it stays put when zooming.
  const hue = (i: number): number => (i * 47) % 360;
  const st = rows.map((s) => ({ ...s, py: y(s.y) })).filter((s) => s.py >= m.t - 1 && s.py <= H - m.b + 1);
  const bands = el("g", { "clip-path": "url(#area)" }, svg);
  st.forEach((s, k) => {
    const prev = st[k - 1],
      next = st[k + 1];
    const above = prev ? (prev.py + s.py) / 2 : m.t;
    const below = next ? (s.py + next.py) / 2 : H - m.b;
    el(
      "rect",
      {
        x: m.l,
        y: above,
        width: W - m.l - m.r,
        height: Math.max(0, below - above),
        fill: `hsl(${String(hue(s.i))}, 60%, 50%)`,
        "fill-opacity": 0.05,
      },
      bands,
    );
  });

  // Every visible station gets a label: labels of nearby stations are pushed
  // apart and joined to their row by a leader line in the row's band colour.
  const gap = Math.min(12, (H - m.t - m.b) / Math.max(1, st.length));
  const at = st.map((s) => s.py);
  for (let k = 1; k < at.length; k++) at[k] = Math.max(item(at, k), item(at, k - 1) + gap);
  const lastAt = at[at.length - 1];
  if (lastAt !== undefined && lastAt > H - m.b) {
    at[at.length - 1] = H - m.b;
    for (let k = at.length - 2; k >= 0; k--) at[k] = Math.min(item(at, k), item(at, k + 1) - gap);
  }
  const stations = el("g", { class: "station" }, svg);
  st.forEach((s, k) => {
    const ak = item(at, k);
    el("line", { x1: m.l, x2: W - m.r, y1: s.py, y2: s.py }, stations);
    el(
      "polyline",
      {
        points: `${String(m.l - 26)},${String(ak)} ${String(m.l - 8)},${String(s.py)} ${String(m.l)},${String(s.py)}`,
        class: "leader",
        stroke: `hsl(${String(hue(s.i))}, 60%, 50%)`,
      },
      stations,
    );
    const t = el(
      "text",
      { x: m.l - 30, y: ak + gap / 3, "text-anchor": "end", "font-size": Math.min(11, gap), class: "station-name" },
      stations,
    );
    t.textContent = s.name.length > 32 ? s.name.slice(0, 31) + "…" : s.name;
    // Like a line in the plot: hovering previews what passes through the
    // station at any time, clicking lists the connections touching it.
    t.addEventListener("mousemove", (e) => {
      if (drag) return;
      hoverTarget = md.stationTarget(s.i);
      refresh();
      moveTip(e);
    });
    t.addEventListener("mouseleave", () => {
      hoverTarget = null;
      refresh();
    });
    t.addEventListener("click", (e) => {
      e.stopPropagation();
      if (!dragged) choose(md.stationTarget(s.i));
    });
  });

  // Time ticks: the coarsest step that still gives about ten labels.
  const axis = el("g", { class: "axis" }, svg);
  const steps = [5, 10, 15, 30, 60, 120, 180, 360].map((mins) => mins * 60e3);
  const step = steps.find((s) => (dom.t1 - dom.t0) / s <= 12) ?? item(steps, steps.length - 1);
  for (let t = Math.ceil(dom.t0 / step) * step; t <= dom.t1; t += step) {
    el("line", { x1: x(t), x2: x(t), y1: m.t, y2: H - m.b }, axis);
    el("text", { x: x(t), y: H - 10, "text-anchor": "middle" }, axis).textContent = timeFmt.format(t);
  }

  const pts = (p: { pts: readonly XY[] }): string =>
    p.pts.map(([t, v]) => `${x(t).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const base = el("g", { class: "base", "clip-path": "url(#area)" }, svg);
  for (const e of md.edges) el("polyline", { points: pts(e), class: "ride", stroke: trainColor(e.trip) }, base);
  for (const w of md.waits) {
    for (const [from, to] of w.runs)
      el(
        "polyline",
        {
          points: pts({
            pts: [
              [from, w.y],
              [to, w.y],
            ],
          }),
          class: "wait",
        },
        base,
      );
  }
  for (const pi of md.warns) el("polyline", { points: pts(item(D.pieces, pi)), class: "wait warn" }, base);
  for (const pi of md.walks) el("polyline", { points: pts(item(D.pieces, pi)), class: "walk" }, base);
  const overlay = el("g", { class: "overlay", "clip-path": "url(#area)" }, svg);

  const hits = el("g", { "clip-path": "url(#area)" }, svg);
  md.targets.forEach((t, i) => el("polyline", { points: pts(t), class: "hit", "data-t": i }, hits));
  const timeAt = (e: MouseEvent): number =>
    dom.t0 + ((e.clientX - svg.getBoundingClientRect().left - m.l) / (W - m.l - m.r)) * (dom.t1 - dom.t0);
  // Hit lines are 9 px wide, so lines of nearby stations (Ludwigshafen Mitte
  // 2 min above Mannheim Hbf) overlap: the line nearest to the mouse wins, not
  // the one drawn last.
  const nearest = (e: MouseEvent, key: string): number | null => {
    const r = svg.getBoundingClientRect(),
      px = e.clientX - r.left,
      py = e.clientY - r.top;
    let best: number | null = null,
      bestD = Infinity;
    for (const h of document.elementsFromPoint(e.clientX, e.clientY)) {
      if (!(h instanceof SVGPolylineElement)) continue;
      const i = h.dataset[key];
      if (i === undefined) continue;
      const p = [...h.points];
      for (let k = 0; k < p.length; k++) {
        const a = item(p, k),
          b = item(p, Math.min(k + 1, p.length - 1));
        const dx = b.x - a.x,
          dy = b.y - a.y,
          len = dx * dx + dy * dy;
        const u = len ? Math.max(0, Math.min(1, ((px - a.x) * dx + (py - a.y) * dy) / len)) : 0;
        const d = Math.hypot(px - a.x - u * dx, py - a.y - u * dy);
        if (d < bestD) {
          bestD = d;
          best = Number(i);
        }
      }
    }
    return best;
  };
  const targetAt = (e: MouseEvent): Target | null => {
    const i = nearest(e, "t");
    return i === null ? null : item(md.targets, i).at(timeAt(e));
  };
  hits.addEventListener("mousemove", (e) => {
    if (drag) return;
    hoverTarget = targetAt(e);
    refresh();
    moveTip(e);
  });
  hits.addEventListener("mouseout", () => {
    hoverTarget = null;
    refresh();
  });
  hits.addEventListener("click", (e) => {
    e.stopPropagation();
    if (dragged) return;
    const t = targetAt(e);
    if (t) choose(t);
  });

  const hoverLayer = el("g", { "clip-path": "url(#area)" }, svg);
  const fbLayer = el("g", { class: "fallback", "clip-path": "url(#area)" }, svg);
  const fbTargetAt = (e: MouseEvent): Hover | null => {
    const i = nearest(e, "f");
    return i === null ? null : (view?.fbTargets[i]?.at(timeAt(e)) ?? null);
  };
  fbLayer.addEventListener("mousemove", (e) => {
    if (drag) return;
    hoverTarget = fbTargetAt(e);
    refresh();
    moveTip(e);
  });
  fbLayer.addEventListener("mouseout", () => {
    hoverTarget = null;
    refresh();
  });
  // Clicking a fallback keeps the selection it belongs to.
  fbLayer.addEventListener("click", (e) => {
    e.stopPropagation();
  });
  const labels = el("g", { class: "change-labels", "clip-path": "url(#area)" }, svg);
  svg.addEventListener("click", () => {
    if (dragged) return;
    clearPicks();
    refresh();
  });

  view = { svg, x, y, pts, overlay, hoverLayer, fbLayer, labels, overlayKey: null, fbKey: undefined, fbTargets: [] };
  refresh();
}

// Zoom and pan: wheel zooms around the mouse (Shift: time only, Alt: height
// only), dragging pans, double-click or the button resets.
let frame: number | null = null;
function scheduleRender(): void {
  frame ??= requestAnimationFrame(() => {
    frame = null;
    render();
  });
}
function clampDomain(): void {
  const minT = 10 * 60e3,
    minY = (full.y1 - full.y0) / 200;
  if (dom.t1 - dom.t0 < minT) {
    const c = (dom.t0 + dom.t1) / 2;
    dom.t0 = c - minT / 2;
    dom.t1 = c + minT / 2;
  }
  if (dom.y1 - dom.y0 < minY) {
    const c = (dom.y0 + dom.y1) / 2;
    dom.y0 = c - minY / 2;
    dom.y1 = c + minY / 2;
  }
  const span = (lo: number, hi: number, a: number, b: number): Interval => {
    // Never wider than everything, and never off it.
    if (hi - lo > b - a) return [a, b];
    if (lo < a) return [a, a + hi - lo];
    if (hi > b) return [b - (hi - lo), b];
    return [lo, hi];
  };
  [dom.t0, dom.t1] = span(dom.t0, dom.t1, full.t0, full.t1);
  [dom.y0, dom.y1] = span(dom.y0, dom.y1, full.y0, full.y1);
}
function mouseInPlot(e: MouseEvent, { W, H, m }: Geom): { px: number; py: number; fx: number; fy: number } {
  const r = plotEl.getBoundingClientRect();
  const px = e.clientX - r.left,
    py = e.clientY - r.top;
  return { px, py, fx: (px - m.l) / (W - m.l - m.r), fy: (py - m.t) / (H - m.t - m.b) };
}
plotEl.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    if (!geom) return;
    const { fx, fy } = mouseInPlot(e, geom);
    const k = Math.exp(e.deltaY * 0.0015);
    if (!e.altKey) {
      const at = dom.t0 + fx * (dom.t1 - dom.t0);
      dom.t0 = at - (at - dom.t0) * k;
      dom.t1 = at + (dom.t1 - at) * k;
    }
    if (!e.shiftKey) {
      const at = dom.y1 - fy * (dom.y1 - dom.y0);
      dom.y0 = at - (at - dom.y0) * k;
      dom.y1 = at + (dom.y1 - at) * k;
    }
    clampDomain();
    scheduleRender();
  },
  { passive: false },
);
let drag: { x: number; y: number; dom: Domain } | null = null,
  dragged = false;
plotEl.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  drag = { x: e.clientX, y: e.clientY, dom: { ...dom } };
  dragged = false;
});
window.addEventListener("mousemove", (e) => {
  if (!drag || !geom) return;
  const dx = e.clientX - drag.x,
    dy = e.clientY - drag.y;
  if (!dragged && Math.hypot(dx, dy) < 4) return;
  dragged = true;
  tip.style.display = "none";
  const { W, H, m } = geom;
  const dt = (dx / (W - m.l - m.r)) * (drag.dom.t1 - drag.dom.t0);
  const dv = (dy / (H - m.t - m.b)) * (drag.dom.y1 - drag.dom.y0);
  dom = { t0: drag.dom.t0 - dt, t1: drag.dom.t1 - dt, y0: drag.dom.y0 + dv, y1: drag.dom.y1 + dv };
  clampDomain();
  scheduleRender();
});
window.addEventListener("mouseup", () => {
  drag = null;
  // The click that ends a drag must not select or clear; let it pass first.
  if (dragged)
    setTimeout(() => {
      dragged = false;
    }, 0);
});
plotEl.addEventListener("dblclick", () => {
  dom = { ...full };
  scheduleRender();
});
byId("reset", HTMLButtonElement).addEventListener("click", () => {
  dom = { ...full };
  scheduleRender();
});

// Clicking a line or station picks it; clicking it again unpicks it. Several
// picks narrow down: the listed connections pass all of them, the highlight is
// what is reachable through each. One connection left is selected directly.
function choose(target: Target | Picked): void {
  const k = picks.findIndex((p) => p.id === target.id);
  if (k >= 0) picks.splice(k, 1);
  else picks.push({ id: target.id, info: target.info, chains: target.chains, reach: target.reach ?? null });
  applyPicks();
  refresh();
}
function applyPicks(): void {
  const [first, ...rest] = picks;
  if (!first) {
    candidates = null;
    pinned = null;
    return;
  }
  let chains = first.chains.filter((i) => !hidden.has(item(D.chains, i).group));
  for (const p of rest) {
    const s = new Set(p.chains);
    chains = chains.filter((i) => s.has(i));
  }
  // Reachable through every pick: per piece, the stretch all their highlights share.
  const reaches = picks.map((p) => p.reach).filter((r): r is Reach => r !== null);
  const reach = reaches.length
    ? {
        key: picks.map((p) => p.reach?.key ?? p.id).join("|"),
        segments: intersectReach(reaches.map((r) => r.segments)),
      }
    : null;
  candidates = { info: picks.map((p) => p.info).join(" + "), chains, reach };
  pinned = chains.length === 1 ? item(chains, 0) : pinned !== null && chains.includes(pinned) ? pinned : null;
}
function clearPicks(): void {
  picks = [];
  candidates = null;
  pinned = null;
  preview = null;
}

function refresh(): void {
  if (!view) return;
  const { svg, x, y, pts, overlay, hoverLayer, fbLayer, labels } = view;
  const strong = [pinned, preview].filter((i): i is number => i !== null);
  // Hovered stretch and hovered group show at full strength; the rest of a
  // clicked list recedes once one of its connections is selected or previewed.
  const soft = new Set<number>(),
    faded = new Set<number>();
  // Hovering shows what is reachable through the point; a clicked point keeps
  // showing it, receding once one of its connections is selected or previewed.
  const reach = hoverTarget?.reach ?? candidates?.reach ?? null;
  const reachFaded = !hoverTarget?.reach && strong.length > 0;
  if (hoverTarget && !hoverTarget.reach) hoverTarget.chains.forEach((i) => soft.add(i));
  if (hoverTarget?.reach?.with) hoverTarget.reach.with.forEach((i) => soft.add(i));
  if (softGroup !== null)
    D.chains.forEach((c, i) => {
      if (c.group === softGroup && !hidden.has(c.group)) soft.add(i);
    });
  if (candidates) candidates.chains.forEach((i) => (strong.length ? faded : soft).add(i));
  svg.classList.toggle("focus", strong.length > 0 || soft.size > 0 || faded.size > 0 || reach !== null);

  // Overlay: previewed connections in train colours, the selection thick on top.
  // Redrawn only when the set changes, not on every mouse move.
  const key = JSON.stringify([
    strong,
    [...soft].sort((a, b) => a - b),
    [...faded].sort((a, b) => a - b),
    reach?.key,
    reachFaded,
  ]);
  if (key !== view.overlayKey) {
    view.overlayKey = key;
    overlay.replaceChildren();
    // A piece, or its stretch between two times (reach highlights).
    const draw = (pi: number, cls: string, from = -Infinity, to = Infinity): void => {
      const p = item(D.pieces, pi);
      let s = p.pts;
      if (from > -Infinity || to < Infinity) {
        // Each edge clipped to [from, to]; time runs forward along a piece.
        const at = (a: Pt, b: Pt, t: number): Pt => [t, a[1] + ((b[1] - a[1]) * (t - a[0])) / (b[0] - a[0]), a[2]];
        const cut: Pt[] = [];
        const push = (q: Pt): void => {
          const l = cut[cut.length - 1];
          if (l?.[0] !== q[0] || l[1] !== q[1]) cut.push(q);
        };
        for (let k = 1; k < s.length; k++) {
          const a = item(s, k - 1),
            b = item(s, k);
          if (b[0] < from || a[0] > to) continue;
          if (a[0] === b[0]) {
            push(a);
            push(b);
            continue;
          }
          push(at(a, b, Math.max(a[0], from)));
          push(at(a, b, Math.min(b[0], to)));
        }
        s = cut;
      }
      if (s.length < 2 && p.pts.length >= 2) return;
      const line = el(
        "polyline",
        { points: pts({ pts: s }), class: `${p.kind}${p.warn ? " warn" : ""} ${cls}` },
        overlay,
      );
      if (p.kind === "ride" && p.trip !== null) line.setAttribute("stroke", trainColor(p.trip));
    };
    const mine = new Set(strong.flatMap((i) => item(D.chains, i).pieces));
    const shown = new Set(mine);
    const pieces = (chains: Iterable<number>): Set<number> =>
      new Set([...chains].flatMap((i) => item(D.chains, i).pieces).filter((pi) => !shown.has(pi)));
    const bright = pieces(soft);
    bright.forEach((pi) => shown.add(pi));
    if (reach && reachFaded)
      for (const [pi, ...cut] of reach.segments) if (!shown.has(pi)) draw(pi, "soft faded", ...cut);
    for (const pi of pieces(faded)) draw(pi, "soft faded");
    for (const pi of bright) draw(pi, "soft");
    if (reach && !reachFaded) for (const [pi, ...cut] of reach.segments) if (!shown.has(pi)) draw(pi, "soft", ...cut);
    for (const pi of mine) draw(pi, "strong");
  }

  hoverLayer.replaceChildren();
  if (hoverTarget) el("polyline", { points: pts(hoverTarget), class: "hover-piece" }, hoverLayer);

  // Fallbacks of the selection, dashed in train colours, hoverable. Redrawn
  // only when the selection changes, so hovering them doesn't replace them.
  const fbKey = (focus: number | null): number | null => (focus === pinned ? focus : null);
  const focus = preview ?? pinned;
  const fk = fbKey(focus);
  if (fk !== view.fbKey) {
    view.fbKey = fk;
    fbLayer.replaceChildren();
    const fbTargets: FbTarget[] = [];
    view.fbTargets = fbTargets;
    if (fk !== null) {
      // Fallbacks share stretches (the same S1, the wait after the missed
      // train): one target per stretch and per continuous stay, naming all.
      const byPiece = new Map<Piece, Set<Fallback>>(),
        waits = new Map<number, { y: number; spans: { from: number; to: number; f: Fallback }[] }>();
      for (const f of item(D.chains, fk).fallbacks) {
        const g = el("g", {}, fbLayer);
        for (const p of f.pieces) {
          const line = el("polyline", { points: pts(p), class: `${p.kind}${p.warn ? " warn" : ""}` }, g);
          if (p.kind === "ride" && p.trip !== null) line.setAttribute("stroke", trainColor(p.trip));
          const first = p.pts[0],
            last = p.pts[p.pts.length - 1];
          if (!first || !last) continue;
          if (p.kind === "wait") {
            const [from, wy, s] = first,
              [to] = last;
            entry(waits, s, () => ({ y: wy, spans: [] })).spans.push({ from, to, f });
          } else {
            entry(byPiece, p, () => new Set()).add(f);
          }
        }
      }
      const hit = (points: Pt[], at: (time: number) => Hover): void => {
        fbTargets.push({ at });
        el("polyline", { points: pts({ pts: points }), class: "hit", "data-f": fbTargets.length - 1 }, fbLayer);
      };
      for (const [p, fs] of byPiece) {
        const t: Hover = {
          info: p.info,
          ...(p.trip === null ? {} : { trip: p.trip }),
          chains: [],
          fallbacks: [...fs],
          pts: p.pts,
        };
        hit(p.pts, () => t);
      }
      for (const [s, { y: wy, spans }] of waits) {
        for (const [from, to] of runsOf(spans))
          hit(
            [
              [from, wy, s],
              [to, wy, s],
            ],
            (time) => {
              const { t, here, pts: stayPts } = stayAt(spans, from, to, time, wy, s);
              return {
                info: `waiting at ${item(D.names, s)} · ${timeFmt.format(t)}`,
                chains: [],
                fallbacks: [...new Set(here.map((w) => w.f))],
                pts: stayPts,
              };
            },
          );
      }
    }
  }
  labels.replaceChildren();
  if (focus !== null) {
    // Name the changes: their station rows are often too close to tell apart.
    for (const p of item(D.chains, focus)
      .pieces.map((i) => item(D.pieces, i))
      .filter((p) => p.kind === "wait")) {
      const first = p.pts[0];
      if (!first) continue;
      const [t, v] = first;
      el("text", { x: x(t) + 4, y: y(v) - 5 }, labels).textContent = p.info;
    }
  }

  if (hoverTarget) {
    const { info, chains, trip } = hoverTarget;
    const swatch = trip === undefined ? "" : `<span style="color:${trainColor(trip)}">■</span> `;
    let who: string;
    const only = chains.length === 1 ? chains[0] : undefined;
    if (hoverTarget.fallbacks) {
      const at = [...new Set(hoverTarget.fallbacks.map((f) => f.at))];
      who =
        `<br><span style="color:var(--accent)">If the change at ${esc(at.join(" / "))} is missed:</span>` +
        hoverTarget.fallbacks
          .map((f) => `<br><span style="color:var(--muted)">• ${esc(f.line)} · arrive ${esc(f.arrive)}</span>`)
          .join("");
    } else if (only !== undefined) {
      const c = item(D.chains, only);
      who = `<br><b>${esc(c.header)}</b><span style="color:var(--muted)">${esc(c.route)}</span>`;
    } else {
      const groups = new Map<number, number>();
      for (const i of chains) {
        const g = item(D.chains, i).group;
        groups.set(g, (groups.get(g) ?? 0) + 1);
      }
      const list = [...groups]
        .sort((a, b) => b[1] - a[1])
        .map(([g, n]) => `• ${esc(item(D.groups, g).label)} (${String(n)})`)
        .join("<br>");
      who = `<br><span style="color:var(--muted)">${String(chains.length)} connections in ${String(groups.size)} group${groups.size > 1 ? "s" : ""} · click to list them</span><br>${list}`;
    }
    tip.innerHTML = `<b>${swatch}${esc(info)}</b>${who}`;
    tip.style.display = "block";
  } else {
    tip.style.display = "none";
  }
  renderPanel();
}

function moveTip(e: MouseEvent): void {
  const r = plotEl.getBoundingClientRect();
  tip.style.left = `${String(Math.min(e.clientX - r.left + 14, r.width - tip.offsetWidth - 8))}px`;
  tip.style.top = `${String(e.clientY - r.top + 14)}px`;
}

let panelKey: string | null = null; // avoid rebuilding the panel (and losing its hover) on every mouse move
function renderPanel(): void {
  const key = JSON.stringify([pinned, picks.map((p) => p.id), candidates?.chains.length ?? null]);
  if (key === panelKey) return;
  panelKey = key;
  // The picks, each unpicked by clicking it (or its line again).
  picksEl.replaceChildren();
  for (const p of picks) {
    const row = document.createElement("div");
    row.className = "pick";
    row.title = "Click to remove";
    row.innerHTML = `<span>${esc(p.info)}</span><span class="x">×</span>`;
    row.addEventListener("click", () => {
      choose(p);
    });
    picksEl.appendChild(row);
  }
  if (picks.length) {
    const hint = document.createElement("div");
    hint.className = "pick-hint";
    hint.textContent = "Click more lines or stations to narrow down; Esc clears.";
    picksEl.appendChild(hint);
  }
  if (pinned !== null) {
    panelTitle.textContent = "Selected connection";
    const c = item(D.chains, pinned);
    let html =
      candidates && candidates.chains.length > 1
        ? `<span class="back" id="back">← all ${String(candidates.chains.length)} connections</span>`
        : "";
    html += `<b>${esc(c.header)}</b>\n${esc(c.route).replaceAll(" ‖", "\n‖").replaceAll("‖ ", "‖\n")}`;
    let at: string | null = null;
    for (const f of c.fallbacks) {
      if (f.at !== at) {
        html += `\n\n<span class="fb">If the change at ${esc(f.at)} is missed:</span>`;
        at = f.at;
      }
      html += `\n<span class="fb">• ${esc(f.line)} · arrive ${esc(f.arrive)}</span>`;
    }
    details.innerHTML = html;
    document.getElementById("back")?.addEventListener("click", () => {
      pinned = null;
      refresh();
    });
  } else if (candidates) {
    panelTitle.textContent =
      picks.length > 1
        ? `${String(candidates.chains.length)} connections pass all ${String(picks.length)}`
        : `${String(candidates.chains.length)} connections use it`;
    details.replaceChildren();
    for (const i of candidates.chains) {
      const c = item(D.chains, i);
      const row = document.createElement("div");
      row.className = "candidate";
      row.innerHTML = `<b>${esc(c.header)}</b><div class="route">${esc(c.route)}</div>`;
      row.addEventListener("mouseenter", () => {
        preview = i;
        refresh();
      });
      row.addEventListener("mouseleave", () => {
        preview = null;
        refresh();
      });
      row.addEventListener("click", () => {
        preview = null;
        pinned = i;
        refresh();
      });
      details.appendChild(row);
    }
  } else {
    panelTitle.textContent = "Selection";
    details.textContent = HINT;
  }
}

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    clearPicks();
    refresh();
    return;
  }
  if (!candidates || (e.key !== "ArrowDown" && e.key !== "ArrowUp")) return;
  e.preventDefault();
  const list = candidates.chains;
  const now = pinned === null ? -1 : list.indexOf(pinned);
  const next = now < 0 ? 0 : (now + (e.key === "ArrowDown" ? 1 : list.length - 1)) % list.length;
  // An empty list has nothing to step to.
  pinned = list[next] ?? null;
  refresh();
});

const groupsEl = byId("groups", HTMLDivElement);
D.groups.forEach((g, i) => {
  const row = document.createElement("div");
  row.className = "group";
  row.innerHTML = `<span>${esc(g.label)}</span><span class="count">${String(g.count)}</span>`;
  row.addEventListener("mouseenter", () => {
    softGroup = hidden.has(i) ? null : i;
    refresh();
  });
  row.addEventListener("mouseleave", () => {
    softGroup = null;
    refresh();
  });
  row.addEventListener("click", () => {
    if (hidden.has(i)) hidden.delete(i);
    else hidden.add(i);
    row.classList.toggle("off", hidden.has(i));
    softGroup = null;
    if (pinned !== null && item(D.chains, pinned).group === i) pinned = null;
    applyPicks();
    panelKey = null;
    buildModel();
    render();
  });
  groupsEl.appendChild(row);
});

buildModel();
new ResizeObserver(render).observe(plotEl);
