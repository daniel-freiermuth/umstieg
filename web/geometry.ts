// Plot geometry without the DOM: clipping lines to time intervals, the visible
// domain under zoom and pan, tick steps, label placement, hit distances.

import { item } from "./util";

/** A point in the plot: (ms, height, station). */
export type Pt = [t: number, y: number, s: number];
/** Anything drawable as a polyline: (ms, height, …). */
export type XY = readonly [number, number, ...number[]];
export type Interval = [from: number, to: number];

/** Visible part of the plot: times t0..t1, heights y0..y1. */
export interface Domain {
  t0: number;
  t1: number;
  y0: number;
  y1: number;
}
export interface Margins {
  l: number;
  r: number;
  t: number;
  b: number;
}
/** Plot area: size and margins in pixels. */
export interface Geom {
  W: number;
  H: number;
  m: Margins;
}

/**
 * The stretch of a piece between two times, for reach highlights: each edge
 * clipped to [from, to], interpolating the height; time runs forward along a
 * piece. Vertical edges (no time passes) are kept whole when they touch the
 * interval. Unbounded on both sides, the piece is returned as is.
 */
export function clipLine(pts: readonly Pt[], from: number, to: number): readonly Pt[] {
  if (from === -Infinity && to === Infinity) return pts;
  const at = (a: Pt, b: Pt, t: number): Pt => [t, a[1] + ((b[1] - a[1]) * (t - a[0])) / (b[0] - a[0]), a[2]];
  const cut: Pt[] = [];
  const push = (q: Pt): void => {
    const l = cut[cut.length - 1];
    if (l?.[0] !== q[0] || l[1] !== q[1]) cut.push(q);
  };
  for (let k = 1; k < pts.length; k++) {
    const a = item(pts, k - 1),
      b = item(pts, k);
    if (b[0] < from || a[0] > to) continue;
    if (a[0] === b[0]) {
      push(a);
      push(b);
      continue;
    }
    push(at(a, b, Math.max(a[0], from)));
    push(at(a, b, Math.min(b[0], to)));
  }
  return cut;
}

// Zoomed in at most to 10 minutes and 1/200 of the full height.
const MIN_T = 10 * 60e3;

/**
 * The domain kept within the full extent: at least 10 minutes and 1/200 of the
 * full height wide (around its centre), never wider than everything, and
 * shifted back inside when panned or zoomed off it.
 */
export function clampDomain(dom: Domain, full: Domain): Domain {
  let { t0, t1, y0, y1 } = dom;
  const minY = (full.y1 - full.y0) / 200;
  // Stryker disable next-line EqualityOperator: exactly 10 minutes, re-centring gives the same domain.
  if (t1 - t0 < MIN_T) {
    const c = (t0 + t1) / 2;
    t0 = c - MIN_T / 2;
    t1 = c + MIN_T / 2;
  }
  // Stryker disable next-line EqualityOperator: exactly the minimum height, re-centring gives the same domain.
  if (y1 - y0 < minY) {
    const c = (y0 + y1) / 2;
    y0 = c - minY / 2;
    y1 = c + minY / 2;
  }
  const span = (lo: number, hi: number, a: number, b: number): Interval => {
    // Never wider than everything, and never off it.
    // Stryker disable next-line EqualityOperator: exactly as wide, [a, b] is what the shifts below return too.
    if (hi - lo > b - a) return [a, b];
    // Stryker disable next-line EqualityOperator: at the edge, shifting by nothing gives the same interval.
    if (lo < a) return [a, a + hi - lo];
    // Stryker disable next-line EqualityOperator: at the edge, shifting by nothing gives the same interval.
    if (hi > b) return [b - (hi - lo), b];
    return [lo, hi];
  };
  [t0, t1] = span(t0, t1, full.t0, full.t1);
  [y0, y1] = span(y0, y1, full.y0, full.y1);
  return { t0, t1, y0, y1 };
}

/**
 * Zoom by factor k (> 1: out) around the point at fractions (fx, fy) of the
 * plot area (fy from the top, where the heights are largest); `time` and
 * `height` choose the axes. Clamped to the full extent.
 */
export function zoom(
  dom: Domain,
  full: Domain,
  { fx, fy }: { fx: number; fy: number },
  k: number,
  { time, height }: { time: boolean; height: boolean },
): Domain {
  let { t0, t1, y0, y1 } = dom;
  if (time) {
    const at = t0 + fx * (t1 - t0);
    [t0, t1] = [at - (at - t0) * k, at + (t1 - at) * k];
  }
  if (height) {
    const at = y1 - fy * (y1 - y0);
    [y0, y1] = [at - (at - y0) * k, at + (y1 - at) * k];
  }
  return clampDomain({ t0, t1, y0, y1 }, full);
}

/**
 * The domain dragged by (dx, dy) pixels from where the drag started: the
 * content follows the mouse (right: earlier times, down: larger heights).
 * Clamped to the full extent.
 */
export function pan(start: Domain, full: Domain, dx: number, dy: number, { W, H, m }: Geom): Domain {
  const dt = (dx / (W - m.l - m.r)) * (start.t1 - start.t0);
  const dv = (dy / (H - m.t - m.b)) * (start.y1 - start.y0);
  return clampDomain({ t0: start.t0 - dt, t1: start.t1 - dt, y0: start.y0 + dv, y1: start.y1 + dv }, full);
}

const TICK_STEPS = [5, 10, 15, 30, 60, 120, 180, 360].map((mins) => mins * 60e3);
/** Time tick step for a visible span (ms): the finest giving at most 12 ticks, else 6 hours. */
export function tickStep(span: number): number {
  return TICK_STEPS.find((s) => span / s <= 12) ?? item(TICK_STEPS, TICK_STEPS.length - 1);
}

/**
 * Label positions for rows at ys (ascending pixels, top to bottom): pushed
 * apart to at least `gap`, and if that runs past `bottom`, pushed back up
 * from there.
 */
export function spreadLabels(ys: readonly number[], gap: number, bottom: number): number[] {
  const at = [...ys];
  for (let k = 1; k < at.length; k++) at[k] = Math.max(item(at, k), item(at, k - 1) + gap);
  const lastAt = at[at.length - 1];
  // Stryker disable next-line EqualityOperator: ending exactly at the bottom, pushing up from there changes nothing.
  if (lastAt !== undefined && lastAt > bottom) {
    at[at.length - 1] = bottom;
    for (let k = at.length - 2; k >= 0; k--) at[k] = Math.min(item(at, k), item(at, k + 1) - gap);
  }
  return at;
}

/** Distance from (px, py) to a polyline; a single point counts as such, none is infinitely far. */
export function distanceToLine(px: number, py: number, p: readonly { x: number; y: number }[]): number {
  let best = Infinity;
  for (let k = 0; k < p.length; k++) {
    const a = item(p, k),
      b = item(p, Math.min(k + 1, p.length - 1));
    const dx = b.x - a.x,
      dy = b.y - a.y,
      len = dx * dx + dy * dy;
    const u = len ? Math.max(0, Math.min(1, ((px - a.x) * dx + (py - a.y) * dy) / len)) : 0;
    best = Math.min(best, Math.hypot(px - a.x - u * dx, py - a.y - u * dy));
  }
  return best;
}
