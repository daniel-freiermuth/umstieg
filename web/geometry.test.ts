import { describe, expect, test } from "bun:test";
import {
  type Domain,
  type Geom,
  type Pt,
  clampDomain,
  clipLine,
  distanceToLine,
  pan,
  pinch,
  spreadLabels,
  tickStep,
  zoom,
} from "./geometry";

const MIN = 60e3;

describe("clipLine", () => {
  // A ride from station 1 (height 100) at 0 to station 2 (height 50) at 10,
  // standing there until 14, on to station 3 (height 0) at 24.
  const ride: Pt[] = [
    [0, 100, 1],
    [10, 50, 2],
    [14, 50, 2],
    [24, 0, 3],
  ];

  test("unbounded is the line itself", () => {
    expect(clipLine(ride, -Infinity, Infinity)).toBe(ride);
  });

  test("cuts edges at the bounds, interpolating heights", () => {
    expect(clipLine(ride, 2, 6)).toEqual([
      [2, 90, 1],
      [6, 70, 1],
    ]);
  });

  test("keeps whole edges inside and joins them without repeating points", () => {
    expect(clipLine(ride, 5, 19)).toEqual([
      [5, 75, 1],
      [10, 50, 1],
      [14, 50, 2],
      [19, 25, 2],
    ]);
  });

  test("half-bounded keeps everything on the open side", () => {
    expect(clipLine(ride, 12, Infinity)).toEqual([
      [12, 50, 2],
      [14, 50, 2],
      [24, 0, 2],
    ]);
    expect(clipLine(ride, -Infinity, 12)).toEqual([
      [0, 100, 1],
      [10, 50, 1],
      [12, 50, 2],
    ]);
  });

  test("bounds touching a point keep only that point", () => {
    expect(clipLine(ride, 10, 10)).toEqual([[10, 50, 1]]);
    expect(clipLine(ride, 30, 40)).toEqual([]);
  });

  test("an edge without time (a jump at one instant) is kept whole when it touches", () => {
    const jump: Pt[] = [
      [0, 10, 1],
      [5, 10, 1],
      [5, 20, 2],
      [9, 20, 2],
    ];
    expect(clipLine(jump, 5, 7)).toEqual([
      [5, 10, 1],
      [5, 20, 2],
      [7, 20, 2],
    ]);
    // Touching the interval at its end, or at the piece's start or end.
    expect(clipLine(jump, 3, 5)).toEqual([
      [3, 10, 1],
      [5, 10, 1],
      [5, 20, 2],
    ]);
    expect(clipLine(jump.slice(1), 5, 7)).toEqual([
      [5, 10, 1],
      [5, 20, 2],
      [7, 20, 2],
    ]);
    expect(clipLine(jump.slice(0, 3), 3, 7)).toEqual([
      [3, 10, 1],
      [5, 10, 1],
      [5, 20, 2],
    ]);
  });
});

describe("clampDomain", () => {
  const full: Domain = { t0: 0, t1: 600 * MIN, y0: 0, y1: 1000 };

  test("leaves a domain inside the full extent alone", () => {
    const dom = { t0: 60 * MIN, t1: 120 * MIN, y0: 100, y1: 500 };
    expect(clampDomain(dom, full)).toEqual(dom);
  });

  test("widens to 10 minutes and 1/200 of the height around the centre", () => {
    expect(clampDomain({ t0: 100 * MIN, t1: 102 * MIN, y0: 400, y1: 401 }, full)).toEqual({
      t0: 96 * MIN,
      t1: 106 * MIN,
      y0: 398,
      y1: 403,
    });
  });

  test("never wider than everything", () => {
    expect(clampDomain({ t0: -MIN, t1: 700 * MIN, y0: -5, y1: 2000 }, full)).toEqual(full);
  });

  test("an extent away from zero: its own height and width count", () => {
    const away: Domain = { t0: 600 * MIN, t1: 1200 * MIN, y0: 1000, y1: 2000 };
    // 1/200 of the height is 5.
    expect(clampDomain({ t0: 700 * MIN, t1: 800 * MIN, y0: 1500, y1: 1501 }, away)).toEqual({
      t0: 700 * MIN,
      t1: 800 * MIN,
      y0: 1498,
      y1: 1503,
    });
    // Wider than everything, if less than the sum of its ends.
    expect(clampDomain({ t0: 500 * MIN, t1: 1300 * MIN, y0: 900, y1: 2100 }, away)).toEqual(away);
  });

  test("shifts back inside, keeping the width", () => {
    expect(clampDomain({ t0: -30 * MIN, t1: 30 * MIN, y0: 900, y1: 1100 }, full)).toEqual({
      t0: 0,
      t1: 60 * MIN,
      y0: 800,
      y1: 1000,
    });
    expect(clampDomain({ t0: 590 * MIN, t1: 650 * MIN, y0: -50, y1: 50 }, full)).toEqual({
      t0: 540 * MIN,
      t1: 600 * MIN,
      y0: 0,
      y1: 100,
    });
  });

  test("a widened domain at the edge is shifted inside too", () => {
    expect(clampDomain({ t0: 0, t1: MIN, y0: 0, y1: 1000 }, full)).toEqual({ t0: 0, t1: 10 * MIN, y0: 0, y1: 1000 });
  });
});

describe("zoom", () => {
  const full: Domain = { t0: 0, t1: 600 * MIN, y0: 0, y1: 1000 };
  const dom: Domain = { t0: 100 * MIN, t1: 300 * MIN, y0: 200, y1: 600 };

  test("keeps the point under the mouse in place", () => {
    // A quarter from the left, three quarters from the top: 150 min, height 300.
    const z = zoom(dom, full, { fx: 0.25, fy: 0.75 }, 0.5, { time: true, height: true });
    expect(z).toEqual({ t0: 125 * MIN, t1: 225 * MIN, y0: 250, y1: 450 });
    expect(z.t0 + 0.25 * (z.t1 - z.t0)).toBe(150 * MIN);
    expect(z.y1 - 0.75 * (z.y1 - z.y0)).toBe(300);
  });

  test("zooms only the chosen axes", () => {
    expect(zoom(dom, full, { fx: 0.5, fy: 0.5 }, 0.5, { time: true, height: false })).toEqual({
      ...dom,
      t0: 150 * MIN,
      t1: 250 * MIN,
    });
    expect(zoom(dom, full, { fx: 0.5, fy: 0.5 }, 0.5, { time: false, height: true })).toEqual({
      ...dom,
      y0: 300,
      y1: 500,
    });
  });

  test("zooming out stops at everything", () => {
    expect(zoom(dom, full, { fx: 0.5, fy: 0.5 }, 100, { time: true, height: true })).toEqual(full);
  });
});

describe("pan", () => {
  const full: Domain = { t0: 0, t1: 600 * MIN, y0: 0, y1: 1000 };
  // Plot area 400 × 200 px.
  const geom: Geom = { W: 450, H: 260, m: { l: 40, r: 10, t: 20, b: 40 } };
  const start: Domain = { t0: 100 * MIN, t1: 300 * MIN, y0: 200, y1: 600 };

  test("the content follows the mouse", () => {
    // Right by a quarter: a quarter of the span earlier. Down by half: half higher.
    expect(pan(start, full, 100, 100, geom)).toEqual({ t0: 50 * MIN, t1: 250 * MIN, y0: 400, y1: 800 });
  });

  test("stops at the edges", () => {
    expect(pan(start, full, 1000, -1000, geom)).toEqual({ t0: 0, t1: 200 * MIN, y0: 0, y1: 400 });
  });
});

describe("pinch", () => {
  const full: Domain = { t0: 0, t1: 600 * MIN, y0: 0, y1: 1000 };
  // Plot area 400 × 200 px from (40, 20): its middle is (240, 120).
  const geom: Geom = { W: 450, H: 260, m: { l: 40, r: 10, t: 20, b: 40 } };
  const start: Domain = { t0: 100 * MIN, t1: 300 * MIN, y0: 200, y1: 600 };
  const f = (x: number, y: number): { x: number; y: number } => ({ x, y });

  test("spreading apart sideways zooms the time in around the fingers, the height stays", () => {
    expect(pinch(start, full, [f(190, 120), f(290, 120)], [f(140, 120), f(340, 120)], geom)).toEqual({
      ...start,
      t0: 150 * MIN,
      t1: 250 * MIN,
    });
  });

  test("spreading apart up and down zooms the height in, the time stays", () => {
    expect(pinch(start, full, [f(240, 70), f(240, 170)], [f(240, 20), f(240, 220)], geom)).toEqual({
      ...start,
      y0: 300,
      y1: 500,
    });
  });

  test("diagonally both; pinching together zooms out", () => {
    expect(pinch(start, full, [f(140, 20), f(340, 220)], [f(190, 70), f(290, 170)], geom)).toEqual({
      t0: 0,
      t1: 400 * MIN,
      y0: 0,
      y1: 800,
    });
  });

  test("moving both fingers alike pans like one", () => {
    const moved = pinch(start, full, [f(190, 100), f(290, 140)], [f(290, 200), f(390, 240)], geom);
    expect(moved).toEqual(pan(start, full, 100, 100, geom));
  });

  test("fingers too close along an axis don't zoom it, however they move", () => {
    expect(pinch(start, full, [f(230, 120), f(250, 120)], [f(140, 120), f(340, 120)], geom)).toEqual(start);
    expect(pinch(start, full, [f(140, 120), f(340, 120)], [f(230, 120), f(250, 120)], geom)).toEqual(start);
  });

  test("40 px apart is far enough, before and after", () => {
    // 40 → 80 px: the time span halves; 80 → 40 px: it doubles.
    expect(pinch(start, full, [f(220, 120), f(260, 120)], [f(200, 120), f(280, 120)], geom)).toEqual({
      ...start,
      t0: 150 * MIN,
      t1: 250 * MIN,
    });
    expect(pinch(start, full, [f(200, 120), f(280, 120)], [f(220, 120), f(260, 120)], geom)).toEqual({
      ...start,
      t0: 0,
      t1: 400 * MIN,
    });
  });
});

describe("tickStep", () => {
  const cases: [span: number, step: number][] = [
    [0, 5],
    [60, 5],
    [61, 10],
    [120, 10],
    [180, 15],
    [181, 30],
    [12 * 60, 60],
    [12 * 60 + 1, 120],
    [36 * 60, 180],
    [72 * 60, 360],
    [500 * 60, 360],
  ];
  for (const [span, step] of cases)
    test(`${String(span)} min → every ${String(step)} min`, () => {
      expect(tickStep(span * MIN, 1000)).toBe(step * MIN);
    });

  test("a narrow plot gets as many ticks as labels fit, at least two", () => {
    // 4 hours over 200 px: 4 labels fit, so hourly; over 199 px only 3.
    expect(tickStep(240 * MIN, 200)).toBe(60 * MIN);
    expect(tickStep(240 * MIN, 199)).toBe(120 * MIN);
    expect(tickStep(240 * MIN, 600)).toBe(30 * MIN);
    expect(tickStep(240 * MIN, 650)).toBe(30 * MIN);
    expect(tickStep(60 * MIN, 0)).toBe(30 * MIN);
  });
});

describe("spreadLabels", () => {
  test("rows far enough apart keep their places", () => {
    expect(spreadLabels([10, 30, 50], 12, 100)).toEqual([10, 30, 50]);
  });

  test("close rows are pushed down to the gap", () => {
    expect(spreadLabels([10, 12, 14, 40], 12, 100)).toEqual([10, 22, 34, 46]);
  });

  test("running past the bottom pushes them back up from there", () => {
    expect(spreadLabels([60, 90, 92, 94], 12, 100)).toEqual([60, 76, 88, 100]);
  });

  test("pushed back up as far as needed, the first row too", () => {
    expect(spreadLabels([90, 92, 94], 12, 100)).toEqual([76, 88, 100]);
  });

  test("nothing to place", () => {
    expect(spreadLabels([], 12, 100)).toEqual([]);
  });
});

describe("distanceToLine", () => {
  const line = [
    { x: 0, y: 0 },
    { x: 10, y: 0 },
    { x: 10, y: 10 },
  ];
  const cases: [px: number, py: number, d: number][] = [
    [5, 3, 3], // above the first segment
    [13, 5, 3], // beside the second
    [-3, -4, 5], // beyond the start: to the end point
    [10, 14, 4], // beyond the end
    [10, 0, 0], // on the corner
  ];
  for (const [px, py, d] of cases)
    test(`(${String(px)}, ${String(py)}) is ${String(d)} away`, () => {
      expect(distanceToLine(px, py, line)).toBeCloseTo(d, 9);
    });

  test("wherever the line is", () => {
    const moved = line.map(({ x, y }) => ({ x: x + 7, y: y - 11 }));
    for (const [px, py, d] of cases) expect(distanceToLine(px + 7, py - 11, moved)).toBeCloseTo(d, 9);
  });

  test("a single point and no points", () => {
    expect(distanceToLine(3, 4, [{ x: 0, y: 0 }])).toBe(5);
    expect(distanceToLine(3, 4, [])).toBe(Infinity);
  });
});
