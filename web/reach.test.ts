import { describe, expect, test } from "bun:test";
import type { Interval, Pt } from "./geometry";
import {
  type Line,
  type Route,
  type Segment,
  type Source,
  type Span,
  intersectReach,
  mergeIntervals,
  network,
  reachSegments,
  runsOf,
  stayAt,
} from "./reach";

// Times are plain minutes; heights follow the station.
const pt = (s: number, t: number): Pt => [t, 100 - 10 * s, s];
/** A ride: [station, time] boards or alights, [station, arrival, departure] is a stop in between. */
function ride(trip: number, calls: ([number, number] | [number, number, number])[]): Line {
  return { kind: "ride", warn: false, trip, pts: calls.flatMap(([s, ...ts]) => ts.map((t) => pt(s, t))) };
}
const wait = (s: number, from: number, to: number, warn = false): Line => ({
  kind: "wait",
  warn,
  trip: null,
  pts: [pt(s, from), pt(s, to)],
});
const walk = (a: number, t: number, b: number, t2: number): Line => ({
  kind: "walk",
  warn: false,
  trip: null,
  pts: [pt(a, t), pt(b, t2)],
});

/** Named pieces shared by connections, like the plot's interned pieces. */
class Plot {
  pieces: Line[] = [];
  names: string[] = [];
  chains: Route[] = [];
  constructor(pieces: Record<string, Line>, ...chains: string[][]) {
    for (const [name, line] of Object.entries(pieces)) {
      this.names.push(name);
      this.pieces.push(line);
    }
    this.chains = chains.map((c) => ({ pieces: c.map((name) => this.index(name)) }));
  }
  index(name: string): number {
    const i = this.names.indexOf(name);
    if (i < 0) throw new Error(`no piece ${name}`);
    return i;
  }
  /** Segments as "piece from-to", sorted. */
  show(segments: readonly Segment[]): string[] {
    return segments.map(([pi, a, b]) => `${this.names[pi] ?? "?"} ${String(a)}-${String(b)}`).sort();
  }
  reach(src: Source, visible: (chain: number) => boolean = () => true): string[] {
    return this.show(network(this.pieces, this.chains, visible).reach(src));
  }
}

const [A, B, C, D, E] = [0, 1, 2, 3, 4];

// Two connections changing at B: T1 → T2 waiting 10–30, T3 → T4 waiting 25–50.
const changesAtB = new Plot(
  {
    T1: ride(1, [
      [A, 0],
      [B, 10],
    ]),
    waitB0: wait(B, 10, 30),
    T2: ride(2, [
      [B, 30],
      [C, 40],
    ]),
    T3: ride(3, [
      [D, 15],
      [B, 25],
    ]),
    waitB1: wait(B, 25, 50),
    T4: ride(4, [
      [B, 50],
      [E, 60],
    ]),
  },
  ["T1", "waitB0", "T2"],
  ["T3", "waitB1", "T4"],
);

describe("reach through a station-time", () => {
  test("forward everything leaving from then on, backward everything arriving by then", () => {
    // At B at 12: T3 arrives later (25), so it is not behind; its connection's
    // wait and onward train are ahead.
    expect(changesAtB.reach({ at: [[B, 12]] })).toEqual([
      "T1 0-10",
      "T2 30-40",
      "T4 50-60",
      "waitB0 10-30",
      "waitB1 25-50",
    ]);
  });

  test("trains that left before are not ahead, trains arriving by then are behind", () => {
    expect(changesAtB.reach({ at: [[B, 40]] })).toEqual([
      "T1 0-10",
      "T3 15-25",
      "T4 50-60",
      "waitB0 10-30",
      "waitB1 25-50",
    ]);
  });

  test("waits are clipped to the reachable time", () => {
    // On T3 (D 15 → B 25), as hovering it asks: waiting at B lights up from
    // 25 on, not from 10, where T1's connection starts waiting.
    const onT3: Source = { at: [], back: [[D, 15]], ride: { trip: 3, from: 15, until: 15 } };
    expect(changesAtB.reach(onT3)).toEqual(["T2 30-40", "T4 50-60", "waitB0 25-30", "waitB1 25-50"]);
  });

  test("back only and forward only", () => {
    expect(changesAtB.reach({ at: [], back: [[B, 27]] })).toEqual([
      "T1 0-10",
      "T3 15-25",
      "waitB0 10-27",
      "waitB1 25-27",
    ]);
    expect(changesAtB.reach({ at: [], fwd: [[B, 27]] })).toEqual([
      "T2 30-40",
      "T4 50-60",
      "waitB0 27-30",
      "waitB1 27-50",
    ]);
  });

  test("a station at any time: everything through it", () => {
    // From D only T3 leaves, so T1 (never via D) is out.
    expect(changesAtB.reach({ at: [], fwd: [[D, -Infinity]], back: [[D, Infinity]] })).toEqual([
      "T2 30-40",
      "T4 50-60",
      "waitB0 25-30",
      "waitB1 25-50",
    ]);
  });
});

describe("reach is transitive over everything plotted", () => {
  // ICE 774 passes Kassel, ICE 772 passes it later; no connection changes
  // there. At Z a third connection waits 45–60 for RB 2.
  const [P, K, M, Z, Y, W] = [5, 6, 7, 8, 13, 14];
  const kassel = (at: number, leave: number): Plot =>
    new Plot(
      {
        ICE774: ride(774, [
          [A, 0],
          [K, 20, 22],
          [M, 40],
        ]),
        ICE772: ride(772, [
          [P, at - 15],
          [K, at, leave],
          [Z, leave + 20],
        ]),
        RB1: ride(1, [
          [Y, 30],
          [Z, 45],
        ]),
        waitZ: wait(Z, 45, 60),
        RB2: ride(2, [
          [Z, 60],
          [W, 70],
        ]),
      },
      ["ICE774"],
      ["ICE772"],
      ["RB1", "waitZ", "RB2"],
    );
  const onIce774: Source = { at: [], back: [[A, 0]], ride: { trip: 774, from: 0, until: 0 } };

  const cases: [string, number, number, string[]][] = [
    ["later train", 30, 32, ["ICE772 30-52", "ICE774 20-40", "RB2 60-70", "waitZ 52-60"]],
    ["leaving as ICE 774 arrives", 19, 20, ["ICE772 20-40", "ICE774 20-40", "RB2 60-70", "waitZ 45-60"]],
    ["left before", 15, 18, ["ICE774 20-40"]],
  ];
  for (const [name, at, leave, expected] of cases)
    test(`changing at a station no connection changes at: ${name}`, () => {
      expect(kassel(at, leave).reach(onIce774)).toEqual(expected);
    });

  // Q → K, walk 5 min to L, S1 L → R at 12; U X → L, wait, S2 L → R at 26.
  const [L, Q, R, X] = [9, 10, 11, 12];
  const walking = (minutes: number): Plot =>
    new Plot(
      {
        ICE774: ride(774, [
          [A, 0],
          [K, 20, 22],
          [M, 40],
        ]),
        RE: ride(1, [
          [Q, 0],
          [K, 5],
        ]),
        walk: walk(K, 5, L, 5 + minutes),
        waitL0: wait(L, 5 + minutes, 12 + minutes),
        S1: ride(2, [
          [L, 12 + minutes],
          [R, 20 + minutes],
        ]),
        U: ride(3, [
          [X, 0],
          [L, 24],
        ]),
        waitL1: wait(L, 24, 26),
        S2: ride(4, [
          [L, 26],
          [R, 35],
        ]),
      },
      ["ICE774"],
      ["RE", "walk", "waitL0", "S1"],
      ["U", "waitL1", "S2"],
    );

  test("walks connect too, if there is time", () => {
    // Arriving at Kassel at 20: at L at 25 walking 5 minutes, at 27 walking 7.
    expect(walking(5).reach(onIce774)).toEqual(["ICE774 20-40", "S2 26-35", "waitL1 25-26"]);
    expect(walking(7).reach(onIce774)).toEqual(["ICE774 20-40"]);
  });

  test("hidden connections don't connect", () => {
    expect(walking(5).reach(onIce774, (chain) => chain !== 1)).toEqual(["ICE774 20-40"]);
  });
});

describe("reachSegments", () => {
  // One connection: wait at A, ride A → B, wait at B, ride B (standing 24–26) → C → D.
  const plot = new Plot(
    {
      waitA: wait(A, 0, 5),
      T1: ride(1, [
        [A, 5],
        [B, 10],
      ]),
      waitB: wait(B, 10, 20),
      T2: ride(2, [
        [B, 20],
        [C, 24, 26],
        [D, 30],
      ]),
    },
    ["waitA", "T1", "waitB", "T2"],
  );
  const span = (j: number, from: number, to: number, k?: number): Span => ({
    from,
    to,
    chain: 0,
    j,
    ...(k === undefined ? {} : { k }),
  });
  const at = (spans: Span[], t: number, back: boolean, fwd: boolean): string[] =>
    plot.show(reachSegments(plot.pieces, plot.chains, [{ spans, t, back, fwd }]));

  test("the earliest cut forward and the latest backward count, in any order", () => {
    const early = { spans: [span(0, 0, 5)], t: 2 },
      late = { spans: [span(2, 10, 20)], t: 15 };
    const both = (items: (typeof early)[], back: boolean, fwd: boolean): string[] =>
      plot.show(
        reachSegments(
          plot.pieces,
          plot.chains,
          items.map((x) => ({ ...x, back, fwd })),
        ),
      );
    const forward = ["T1 5-10", "T2 20-30", "waitA 2-5", "waitB 10-20"];
    expect(both([early, late], false, true)).toEqual(forward);
    expect(both([late, early], false, true)).toEqual(forward);
    const backward = ["T1 5-10", "waitA 0-5", "waitB 10-15"];
    expect(both([early, late], true, false)).toEqual(backward);
    expect(both([late, early], true, false)).toEqual(backward);
  });

  test("a standing train is cut at the stop, its stay clipped to the time", () => {
    // Standing at C 24–26 (the edge to point 2 of T2), at 25.
    expect(at([span(3, 24, 26, 2)], 25, false, true)).toEqual(["T2 25-30"]);
    expect(at([span(3, 24, 26, 2)], 25, true, false)).toEqual(["T1 5-10", "T2 20-25", "waitA 0-5", "waitB 10-20"]);
  });

  test("spans not reachable at the time are no cut", () => {
    // The wait at B ends at 20: nothing ahead of it from 21 on; it starts at
    // 10: nothing behind it at 9.
    expect(at([span(2, 10, 20)], 21, false, true)).toEqual([]);
    expect(at([span(2, 10, 20)], 9, true, false)).toEqual([]);
  });

  test("both halves at one time meet without a gap", () => {
    expect(at([span(2, 10, 20)], 15, true, true)).toEqual(["T1 5-10", "T2 20-30", "waitA 0-5", "waitB 10-20"]);
  });
});

describe("network", () => {
  test("connections on the same train share its stretches", () => {
    const plot = new Plot(
      {
        long: ride(1, [
          [A, 0],
          [B, 10, 12],
          [C, 20],
        ]),
        short: ride(1, [
          [A, 0],
          [B, 10],
        ]),
        other: ride(2, [
          [A, 0],
          [B, 10],
        ]),
      },
      ["long"],
      ["short"],
      ["other"],
    );
    const net = network(plot.pieces, plot.chains, () => true);
    expect(net.rides.map((r) => [r.trip, r.pts[0][0], r.pts[1][0], [...r.chains]])).toEqual([
      [1, 0, 10, [0, 1]],
      [1, 10, 12, [0]],
      [1, 12, 20, [0]],
      [2, 0, 10, [2]],
    ]);
  });

  test("stays: waits and standing trains per station, with their place in the connection", () => {
    const plot = new Plot(
      {
        T1: ride(1, [
          [A, 0],
          [B, 10, 10],
          [C, 20],
        ]),
        waitC: wait(C, 20, 30, true),
        T2: ride(2, [
          [C, 30],
          [D, 40],
        ]),
      },
      ["T1", "waitC", "T2"],
    );
    const net = network(plot.pieces, plot.chains, () => true);
    // A stop without standing time is a stay too.
    expect(net.stays.get(B)?.spans).toEqual([{ from: 10, to: 10, chain: 0, trip: 1, j: 0, k: 2 }]);
    expect(net.stays.get(C)?.spans).toEqual([{ from: 20, to: 30, chain: 0, j: 1 }]);
    expect([...net.stays.keys()].sort()).toEqual([B, C]);
    expect(net.warns).toEqual([plot.index("waitC")]);
  });

  test("overlapping and touching waits at a station make one run", () => {
    const plot = new Plot(
      { w0: wait(B, 10, 20), w1: wait(B, 15, 25), w2: wait(B, 25, 30), w3: wait(B, 40, 50), w4: wait(C, 0, 5) },
      ["w0"],
      ["w1"],
      ["w2"],
      ["w3"],
      ["w4"],
    );
    const net = network(plot.pieces, plot.chains, () => true);
    expect(net.waits.map((w) => [w.y, w.runs])).toEqual([
      [
        90,
        [
          [10, 30],
          [40, 50],
        ],
      ],
      [80, [[0, 5]]],
    ]);
  });

  test("hidden connections are left out everywhere", () => {
    const visible = (chain: number): boolean => chain !== 1;
    const net = network(changesAtB.pieces, changesAtB.chains, visible);
    expect(net.rides.map((r) => r.trip)).toEqual([1, 2]);
    expect(net.stays.get(B)?.spans.map((s) => s.chain)).toEqual([0]);
    expect([...net.touching].map(([s, cs]) => [s, [...cs]]).sort()).toEqual([
      [A, [0]],
      [B, [0]],
      [C, [0]],
    ]);
  });

  test("walks list the connections walking them; stations touched by any point", () => {
    const plot = new Plot(
      {
        T1: ride(1, [
          [A, 0],
          [B, 10],
        ]),
        w: walk(B, 10, C, 15),
      },
      ["T1", "w"],
      ["w"],
    );
    const net = network(plot.pieces, plot.chains, () => true);
    expect([...net.walks].map(([pi, cs]) => [pi, [...cs]])).toEqual([[plot.index("w"), [0, 1]]]);
    expect([...net.touching].map(([s, cs]) => [s, [...cs]])).toEqual([
      [A, [0]],
      [B, [0, 1]],
      [C, [0, 1]],
    ]);
  });
});

describe("runs and stays", () => {
  test("runsOf merges overlapping and touching spans, in time order", () => {
    const spans = [
      { from: 40, to: 50 },
      { from: 10, to: 20 },
      { from: 20, to: 25 },
      { from: 12, to: 18 },
      { from: 26, to: 30 },
    ];
    expect(runsOf(spans)).toEqual([
      [10, 25],
      [26, 30],
      [40, 50],
    ]);
  });

  test("stayAt clamps the time into the run and finds the spans covering it", () => {
    const spans = [
      { from: 10, to: 20, id: "a" },
      { from: 15, to: 30, id: "b" },
    ];
    const ids = (time: number): [number, string[]] => {
      const { t, here } = stayAt(spans, 10, 30, time, 50, B);
      return [t, here.map((w) => w.id)];
    };
    expect(ids(5)).toEqual([10, ["a"]]);
    expect(ids(15)).toEqual([15, ["a", "b"]]);
    expect(ids(20)).toEqual([20, ["a", "b"]]);
    expect(ids(25)).toEqual([25, ["b"]]);
    expect(ids(99)).toEqual([30, ["b"]]);
    // The highlight is the whole run, whatever the time.
    expect(stayAt(spans, 10, 30, 25, 50, B).pts).toEqual([
      [10, 50, B],
      [30, 50, B],
    ]);
  });
});

// Seeded, so failures reproduce.
function random(seed: number): (n: number) => number {
  let x = seed;
  return (n) => {
    x = (x * 1103515245 + 12345) % 2147483648;
    return Math.floor((x / 2147483648) * n);
  };
}
const covered = (list: readonly Interval[], x: number): boolean => list.some(([a, b]) => a <= x && x <= b);
const randomIntervals = (rnd: (n: number) => number, n: number): Interval[] =>
  Array.from({ length: n }, (): Interval => {
    const a = rnd(20);
    return [a, a + rnd(6)];
  });
// Every half minute of 0..30.
const probes = Array.from({ length: 61 }, (_, i) => i / 2);

describe("interval algebra", () => {
  test("mergeIntervals: sorted, disjoint, same union", () => {
    for (let seed = 1; seed <= 300; seed++) {
      const rnd = random(seed);
      const list = randomIntervals(rnd, rnd(6));
      const runs = mergeIntervals(list);
      for (let k = 1; k < runs.length; k++) expect(runs[k]?.[0] ?? 0).toBeGreaterThan(runs[k - 1]?.[1] ?? 0);
      for (const x of probes) expect(covered(runs, x)).toBe(covered(list, x));
    }
  });

  test("intersectReach: exactly what every list covers, in any order", () => {
    for (let seed = 1; seed <= 300; seed++) {
      const rnd = random(seed);
      const lists = Array.from({ length: 1 + rnd(3) }, () =>
        Array.from({ length: rnd(5) }, (): Segment => {
          const [a, b] = randomIntervals(rnd, 1)[0] ?? [0, 0];
          return [rnd(3), a, b];
        }),
      );
      const of = (segments: readonly Segment[], pi: number): Interval[] =>
        segments.filter((s) => s[0] === pi).map(([, a, b]): Interval => [a, b]);
      const result = intersectReach(lists);
      const reversed = intersectReach([...lists].reverse());
      for (const pi of [0, 1, 2])
        for (const x of probes) {
          const all = lists.every((l) => covered(of(l, pi), x));
          expect(covered(of(result, pi), x)).toBe(all);
          expect(covered(of(reversed, pi), x)).toBe(all);
        }
    }
  });

  test("intersectReach of nothing is nothing", () => {
    expect(intersectReach([])).toEqual([]);
    expect(intersectReach([[[0, 1, 5]], []])).toEqual([]);
  });
});

describe("reach on random timetables", () => {
  // Connections of one to three rides over five stations, waiting between.
  function randomPlot(rnd: (n: number) => number): { pieces: Line[]; chains: Route[] } {
    const pieces: Line[] = [],
      chains: Route[] = [];
    let trip = 0;
    for (let c = 0; c < 6; c++) {
      const ids: number[] = [];
      let s = rnd(5),
        t = rnd(30);
      for (let r = 0, n = 1 + rnd(3); r < n; r++) {
        if (r > 0) {
          const until = t + rnd(10);
          ids.push(pieces.push(wait(s, t, until)) - 1);
          t = until;
        }
        const calls: ([number, number] | [number, number, number])[] = [[s, t]];
        for (let k = 0, stops = 1 + rnd(2); k < stops; k++) {
          s = (s + 1 + rnd(4)) % 5;
          t += 1 + rnd(10);
          if (k < stops - 1) {
            const dep = t + rnd(3);
            calls.push([s, t, dep]);
            t = dep;
          } else calls.push([s, t]);
        }
        ids.push(pieces.push(ride(++trip, calls)) - 1);
      }
      chains.push({ pieces: ids });
    }
    return { pieces, chains };
  }
  // Whether every stretch of `inner` lies within `outer`.
  const within = (inner: readonly Segment[], outer: readonly Segment[]): boolean =>
    inner.every(([pi, a, b]) =>
      [a, (a + b) / 2, b].every((x) => outer.some(([qi, c, d]) => qi === pi && c <= x && x <= d)),
    );

  test("starting earlier reaches at least as much ahead, arriving later at least as much behind", () => {
    let reaching = 0;
    for (let seed = 1; seed <= 200; seed++) {
      const rnd = random(seed);
      const { pieces, chains } = randomPlot(rnd);
      const net = network(pieces, chains, () => true);
      // A station something stays at, so there is something to reach.
      const stations = [...net.stays.keys()];
      const s = stations[rnd(stations.length)] ?? 0,
        t1 = 10 + rnd(30),
        t2 = t1 + rnd(20);
      const ahead = net.reach({ at: [], fwd: [[s, t2]] }),
        behind = net.reach({ at: [], back: [[s, t1]] });
      if (ahead.length && behind.length) reaching++;
      expect({ seed, ok: within(ahead, net.reach({ at: [], fwd: [[s, t1]] })) }).toEqual({ seed, ok: true });
      expect({ seed, ok: within(behind, net.reach({ at: [], back: [[s, t2]] })) }).toEqual({ seed, ok: true });
    }
    // Not vacuous: most cases reach something both ways.
    expect(reaching).toBeGreaterThan(50);
  });

  test("a connection at a station reaches itself onward and backward", () => {
    // Every wait reached at its own time lights up its own connection whole.
    for (let seed = 1; seed <= 200; seed++) {
      const rnd = random(seed);
      const { pieces, chains } = randomPlot(rnd);
      const net = network(pieces, chains, () => true);
      chains.forEach((c, i) => {
        for (const pi of c.pieces) {
          const p = pieces[pi];
          const first = p?.pts[0];
          if (p?.kind !== "wait" || !first) continue;
          const reached = net.reach({ at: [[first[2], first[0]]] });
          const own = c.pieces.map((q): Segment => {
            const pts = pieces[q]?.pts ?? [];
            return [q, pts[0]?.[0] ?? 0, pts[pts.length - 1]?.[0] ?? 0];
          });
          expect({ seed, chain: i, ok: within(own, reached) }).toEqual({ seed, chain: i, ok: true });
        }
      });
    }
  });
});
