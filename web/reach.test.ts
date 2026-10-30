import { describe, expect, test } from "bun:test";
import type { Interval, Pt } from "./geometry";
import {
  type Line,
  type Route,
  type Segment,
  type Source,
  covers,
  intersectReach,
  mergeIntervals,
  network,
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

/**
 * Named pieces shared by connections, like the plot's interned pieces; all
 * of them the network (what is drawn), `chains` the listed connections.
 */
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
  get base(): number[] {
    return this.pieces.map((_, i) => i);
  }
  net(visible: (chain: number) => boolean = () => true): ReturnType<typeof network> {
    return network(this.pieces, this.base, this.chains, visible);
  }
  reach(src: Source): string[] {
    return this.show(this.net().reach(src));
  }
}

const [A, B, C, D, E, F] = [0, 1, 2, 3, 4, 15];

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
    // T3 itself counts too: hovering it, it is part of what is reachable.
    expect(changesAtB.reach(onT3)).toEqual(["T2 30-40", "T3 15-25", "T4 50-60", "waitB0 25-30", "waitB1 25-50"]);
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
      "T3 15-25",
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
    ["later train", 30, 32, ["ICE772 30-52", "ICE774 0-40", "RB2 60-70", "waitZ 52-60"]],
    ["leaving as ICE 774 arrives", 19, 20, ["ICE772 20-40", "ICE774 0-40", "RB2 60-70", "waitZ 45-60"]],
    ["left before", 15, 18, ["ICE774 0-40"]],
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
    expect(walking(5).reach(onIce774)).toEqual(["ICE774 0-40", "S2 26-35", "waitL1 25-26"]);
    expect(walking(7).reach(onIce774)).toEqual(["ICE774 0-40"]);
  });

  test("walks chain, both ways, and the shortest walk between two stations counts", () => {
    const F = 5;
    const plot = new Plot(
      {
        waitA: wait(A, 0, 5),
        T1: ride(1, [
          [A, 5],
          [B, 10],
        ]),
        fast: walk(B, 10, C, 12),
        slow: walk(B, 30, C, 40),
        hop: walk(C, 12, D, 14),
        waitD: wait(D, 14, 20),
        T2: ride(2, [
          [D, 20],
          [E, 30],
        ]),
        T0: ride(3, [
          [F, 0],
          [C, 5],
        ]),
        waitC: wait(C, 5, 15),
        T3: ride(4, [
          [C, 15],
          [E, 25],
        ]),
      },
      ["waitA", "T1", "fast"],
      ["slow"],
      ["hop", "waitD", "T2"],
      ["T0", "waitC", "T3"],
    );
    // On T1: at B at 10, at C at 12 (not 20), at D at 14.
    // The walks themselves count, and "slow" too: at B from 10, one can be there at 30.
    expect(plot.reach({ at: [], back: [[A, 5]], ride: { trip: 1, from: 5, until: 5 } })).toEqual([
      "T1 5-10",
      "T2 20-30",
      "T3 15-25",
      "fast 10-12",
      "hop 12-14",
      "slow 30-40",
      "waitA 0-5",
      "waitC 12-15",
      "waitD 14-20",
    ]);
    // On T2: at D by 20, at C by 18, at B by 16 (not 8), so T1 behind.
    expect(plot.reach({ at: [], back: [[D, 20]], ride: { trip: 2, from: 20, until: 20 } })).toEqual([
      "T0 0-5",
      "T1 5-10",
      "T2 20-30",
      "fast 10-12",
      "hop 12-14",
      "waitA 0-5",
      "waitC 5-15",
      "waitD 14-20",
    ]);
  });

  test("on a train, its later and earlier stretches count, plotted or not in between", () => {
    // Train 7 from A to B and from C to D; no connection rides it from B to C.
    const plot = new Plot(
      {
        waitA: wait(A, 0, 5),
        T7a: ride(7, [
          [A, 5],
          [B, 10],
        ]),
        T7b: ride(7, [
          [C, 20],
          [D, 30],
        ]),
        waitD: wait(D, 30, 40),
        T8: ride(8, [
          [D, 40],
          [E, 50],
        ]),
      },
      ["waitA", "T7a"],
      ["T7b", "waitD", "T8"],
    );
    expect(plot.reach({ at: [], fwd: [[A, 5]] })).toEqual([
      "T7a 5-10",
      "T7b 20-30",
      "T8 40-50",
      "waitA 5-5",
      "waitD 30-40",
    ]);
    expect(plot.reach({ at: [], back: [[E, 50]] })).toEqual([
      "T7a 5-10",
      "T7b 20-30",
      "T8 40-50",
      "waitA 0-5",
      "waitD 30-40",
    ]);
    // On it at B at 10: from there on, and up to there.
    expect(plot.reach({ at: [], ride: { trip: 7, from: 10, until: 10 } })).toEqual([
      "T7a 5-10",
      "T7b 20-30",
      "T8 40-50",
      "waitA 0-5",
      "waitD 30-40",
    ]);
  });

  test("on a train, an earlier boarding (later alighting) than the hovered stretch counts across gaps", () => {
    // Train 9: A 10 → X 15, a gap (no connection rides X–Y), Y 18 → C 25 → E 35.
    const nine = {
      P1: ride(9, [
        [A, 10],
        [X, 15],
      ]),
      P2: ride(9, [
        [Y, 18],
        [C, 25],
      ]),
      P3: ride(9, [
        [C, 25],
        [E, 35],
      ]),
    };
    // Reached at A by 5: from A on, across the gap; hovering it at C (25) too.
    const ahead = new Plot(
      {
        ...nine,
        T1: ride(1, [
          [D, 0],
          [A, 5],
        ]),
      },
      ["T1"],
    );
    expect(ahead.reach({ at: [], fwd: [[D, 0]] })).toEqual(["P1 10-15", "P2 18-25", "P3 25-35", "T1 0-5"]);
    expect(ahead.reach({ at: [], fwd: [[D, 0]], ride: { trip: 9, from: 25, until: 25 } })).toEqual([
      "P1 10-15",
      "P2 18-25",
      "P3 25-35",
      "T1 0-5",
    ]);
    // Leaving it at E (35) leads on: everything before, across the gap; hovering it at A (10) too.
    const behind = new Plot(
      {
        ...nine,
        T2: ride(2, [
          [E, 40],
          [F, 45],
        ]),
      },
      ["T2"],
    );
    expect(behind.reach({ at: [], back: [[F, 45]] })).toEqual(["P1 10-15", "P2 18-25", "P3 25-35", "T2 40-45"]);
    expect(behind.reach({ at: [], back: [[F, 45]], ride: { trip: 9, from: 10, until: 10 } })).toEqual([
      "P1 10-15",
      "P2 18-25",
      "P3 25-35",
      "T2 40-45",
    ]);
  });

  test("stretches taking no time lead onto others at that time, in any order", () => {
    // T2 leaves B at 10, where T1 arrives from A at 10, taking no time; T2 comes first.
    const forward = new Plot(
      {
        T2: ride(2, [
          [B, 10],
          [C, 20],
        ]),
        T1: ride(1, [
          [A, 10],
          [B, 10],
        ]),
      },
      [],
    );
    expect(forward.reach({ at: [], fwd: [[A, 10]] })).toEqual(["T1 10-10", "T2 10-20"]);
    // Backward: T4 reaches C at 20 taking no time from B; T3 reaches B at 20 and comes first.
    const backward = new Plot(
      {
        T3: ride(3, [
          [A, 15],
          [B, 20],
        ]),
        T4: ride(4, [
          [B, 20],
          [C, 20],
        ]),
      },
      [],
    );
    expect(backward.reach({ at: [], back: [[C, 20]] })).toEqual(["T3 15-20", "T4 20-20"]);
  });

  test("walks there and back in no time end", () => {
    const plot = new Plot(
      {
        T1: ride(1, [
          [A, 0],
          [B, 10],
        ]),
        there: walk(B, 10, C, 10),
        back: walk(C, 10, B, 10),
        waitB: wait(B, 10, 20),
        T2: ride(2, [
          [B, 20],
          [D, 30],
        ]),
      },
      ["T1", "there"],
      ["back", "waitB", "T2"],
    );
    expect(plot.reach({ at: [[B, 10]] })).toEqual(["T1 0-10", "T2 20-30", "back 10-10", "there 10-10", "waitB 10-20"]);
  });
});

describe("network", () => {
  test("stretches of the network's rides, with the listed connections riding them", () => {
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
    );
    // Only "long" and "other" are the network here; "short" is a listed connection's piece.
    const net = network(plot.pieces, [plot.index("long"), plot.index("other")], plot.chains, () => true);
    expect(net.rides.map((r) => [r.trip, r.pts[0][0], r.pts[1][0], plot.names[r.piece], [...r.chains]])).toEqual([
      [1, 0, 10, "long", [0, 1]],
      [1, 10, 12, "long", [0]],
      [1, 12, 20, "long", [0]],
      [2, 0, 10, "other", []],
    ]);
  });

  test("stays: waits and standing trains per station; who of the listed is there when", () => {
    const plot = new Plot(
      {
        T1: ride(1, [
          [A, 0],
          [B, 10, 10],
          [C, 20],
        ]),
        waitC: wait(C, 20, 30),
        T2: ride(2, [
          [C, 30],
          [D, 40],
        ]),
      },
      ["T1", "waitC", "T2"],
      ["waitC"],
    );
    const net = plot.net();
    // A stop without standing time is a stay too.
    expect(net.stays.get(B)?.spans).toEqual([{ from: 10, to: 10, trip: 1 }]);
    expect(net.stays.get(C)?.spans).toEqual([{ from: 20, to: 30 }]);
    expect([...net.stays.keys()].sort()).toEqual([B, C]);
    expect(net.waits).toEqual([plot.index("waitC")]);
    expect([
      net.chainsAt(C, 25),
      net.chainsAt(C, 19),
      net.chainsAt(C, 31),
      net.chainsAt(B, 10),
      net.chainsAt(D, 40),
    ]).toEqual([[0, 1], [], [], [0], []]);
  });

  test("hidden connections pass nowhere; the network stays", () => {
    const net = changesAtB.net((chain) => chain !== 1);
    expect(net.rides.map((r) => [r.trip, [...r.chains]])).toEqual([
      [1, [0]],
      [2, [0]],
      [3, []],
      [4, []],
    ]);
    expect(net.chainsAt(B, 27)).toEqual([0]);
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
    const net = plot.net();
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
    expect(intersectReach([[[0, 1, 5]], [[0, 7, 9]]])).toEqual([]);
  });
});

describe("reach on random timetables", () => {
  const base = (pieces: readonly Line[]): number[] => pieces.map((_, i) => i);
  // Connections of one to three rides over five stations, waiting between,
  // sometimes after a walk.
  function randomPlot(rnd: (n: number) => number): { pieces: Line[]; chains: Route[] } {
    const pieces: Line[] = [],
      chains: Route[] = [];
    let trip = 0;
    for (let c = 0; c < 6; c++) {
      const ids: number[] = [];
      let s = rnd(5),
        t = rnd(30);
      for (let r = 0, n = 1 + rnd(3); r < n; r++) {
        if (r > 0 && rnd(3) === 0) {
          const to = (s + 1 + rnd(4)) % 5,
            arrive = t + rnd(5);
          ids.push(pieces.push(walk(s, t, to, arrive)) - 1);
          [s, t] = [to, arrive];
        }
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
      const net = network(pieces, base(pieces), chains, () => true);
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
      const net = network(pieces, base(pieces), chains, () => true);
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

  // The closure the slow way: relax every stretch and walk until nothing
  // changes, then mark what is reached as the fast way does.
  function slowReach(pieces: readonly Line[], chains: readonly Route[], src: Source): Segment[] {
    const net = network(pieces, base(pieces), chains, () => true);
    const ea = new Map<number, number>(),
      ld = new Map<number, number>(),
      boarded = new Map<number, number>(),
      alighted = new Map<number, number>();
    // Keep the earlier (later) time; whether it changed.
    const earlier = (m: Map<number, number>, x: number, t: number): boolean => {
      const v = m.get(x);
      if (v !== undefined && v <= t) return false;
      m.set(x, t);
      return true;
    };
    const later = (m: Map<number, number>, x: number, t: number): boolean => {
      const v = m.get(x);
      if (v !== undefined && v >= t) return false;
      m.set(x, t);
      return true;
    };
    for (const [s, t] of [...src.at, ...(src.fwd ?? [])]) earlier(ea, s, t);
    for (const [s, t] of [...src.at, ...(src.back ?? [])]) later(ld, s, t);
    if (src.ride) {
      boarded.set(src.ride.trip, src.ride.from);
      alighted.set(src.ride.trip, src.ride.until);
    }
    const walks = [...net.walks.keys()].map((pi) => pieces[pi]?.pts ?? []);
    for (let changed = true; changed;) {
      changed = false;
      for (const {
        trip,
        pts: [a, b],
      } of net.rides) {
        const there = ea.get(a[2]),
          on = boarded.get(trip);
        if (there !== undefined && there <= a[0]) changed = earlier(boarded, trip, a[0]) || changed;
        if (on !== undefined && on <= a[0]) changed = earlier(ea, b[2], b[0]) || changed;
        const by = ld.get(b[2]),
          off = alighted.get(trip);
        if (by !== undefined && by >= b[0]) changed = later(alighted, trip, b[0]) || changed;
        if (off !== undefined && off >= b[0]) changed = later(ld, a[2], a[0]) || changed;
      }
      for (const pts of walks) {
        const a = pts[0],
          b = pts[pts.length - 1];
        if (!a || !b) continue;
        const there = ea.get(a[2]),
          by = ld.get(b[2]);
        if (there !== undefined) changed = earlier(ea, b[2], there + b[0] - a[0]) || changed;
        if (by !== undefined) changed = later(ld, a[2], by - b[0] + a[0]) || changed;
      }
    }
    const raw = new Map<number, Interval[]>();
    const add = (pi: number, a: number, b: number): void => {
      raw.set(pi, [...(raw.get(pi) ?? []), [a, b]]);
    };
    for (const {
      trip,
      piece,
      pts: [a, b],
    } of net.rides) {
      const on = boarded.get(trip),
        off = alighted.get(trip);
      if ((on !== undefined && on <= a[0]) || (off !== undefined && off >= b[0])) add(piece, a[0], b[0]);
    }
    for (const pi of net.waits) {
      const pts = pieces[pi]?.pts ?? [];
      const first = pts[0],
        last = pts[pts.length - 1];
      if (!first || !last) continue;
      const there = ea.get(first[2]),
        by = ld.get(first[2]);
      if (there !== undefined && last[0] >= there) add(pi, Math.max(first[0], there), last[0]);
      if (by !== undefined && first[0] <= by) add(pi, first[0], Math.min(last[0], by));
    }
    for (const pi of net.walks.keys()) {
      const pts = pieces[pi]?.pts ?? [];
      const a = pts[0],
        b = pts[pts.length - 1];
      if (!a || !b) continue;
      const there = ea.get(a[2]),
        by = ld.get(b[2]);
      if ((there !== undefined && there <= a[0]) || (by !== undefined && by >= b[0])) add(pi, a[0], b[0]);
    }
    const out: Segment[] = [];
    for (const [pi, list] of raw) for (const [a, b] of mergeIntervals(list)) out.push([pi, a, b]);
    return out;
  }

  test("the same as relaxing every stretch and walk until nothing changes", () => {
    const show = (segments: readonly Segment[]): string[] => segments.map((x) => x.join(" ")).sort();
    let reaching = 0;
    for (let seed = 1; seed <= 200; seed++) {
      const rnd = random(seed);
      const { pieces, chains } = randomPlot(rnd);
      const net = network(pieces, base(pieces), chains, () => true);
      const s = rnd(5),
        t = 10 + rnd(30);
      // On a random stretch, as hovering it asks.
      const r = net.rides[rnd(net.rides.length)];
      const sources: Source[] = [{ at: [[s, t]] }, { at: [], back: [[s, t]] }, { at: [], fwd: [[s, t]] }];
      if (r) {
        const [[t0, , s0]] = r.pts;
        sources.push({ at: [], back: [[s0, t0]], ride: { trip: r.trip, from: t0, until: t0 } });
      }
      for (const src of sources) {
        const fast = net.reach(src);
        if (fast.length) reaching++;
        expect({ seed, src, reach: show(fast) }).toEqual({ seed, src, reach: show(slowReach(pieces, chains, src)) });
      }
    }
    expect(reaching).toBeGreaterThan(400);
  });
});

describe("covers", () => {
  // Train 1 from station 0 (10) to 1 (20), standing there until 25, on to 2 (40); a wait at 1, 20–30.
  const lines = [
    ride(1, [
      [0, 10],
      [1, 20, 25],
      [2, 40],
    ]),
    wait(1, 20, 30),
  ];
  test("a stretch shares time with a segment of its piece", () => {
    const seg: Segment[] = [[0, 15, 22]];
    expect([
      covers(seg, lines, { piece: 0, from: 10, to: 20 }),
      covers(seg, lines, { piece: 0, from: 20, to: 25 }),
      // Touching ends share nothing.
      covers(seg, lines, { piece: 0, from: 5, to: 15 }),
      covers(seg, lines, { piece: 0, from: 22, to: 40 }),
      covers(seg, lines, { piece: 1, from: 20, to: 30 }),
      // Any of several segments will do.
      covers([[1, 0, 5], ...seg], lines, { piece: 0, from: 10, to: 20 }),
    ]).toEqual([true, true, false, false, false, true]);
  });
  test("an instant stretch lies in a segment, ends included", () => {
    const seg: Segment[] = [[0, 15, 22]];
    expect([14, 15, 22, 23].map((t) => covers(seg, lines, { piece: 0, from: t, to: t }))).toEqual([
      false,
      true,
      true,
      false,
    ]);
  });
  test("a moment at a station lies in a wait or a standing train there", () => {
    const waiting: Segment[] = [[1, 20, 28]];
    const standing: Segment[] = [[0, 21, 40]];
    expect([
      covers(waiting, lines, { station: 1, t: 28 }),
      covers(waiting, lines, { station: 1, t: 29 }),
      covers(standing, lines, { station: 1, t: 23 }),
      // Riding on from the station is not being there.
      covers(standing, lines, { station: 1, t: 30 }),
      covers(standing, lines, { station: 2, t: 30 }),
      covers(standing, lines, { station: 0, t: 21 }),
      // Inside the segment's time but before (at) its start.
      covers([[1, 22, 28]], lines, { station: 1, t: 21 }),
      covers([[1, 22, 28]], lines, { station: 1, t: 22 }),
      // In the segment's time, but before the wait there begins.
      covers([[1, 15, 30]], lines, { station: 1, t: 19 }),
      covers(
        [
          [0, 0, 5],
          [1, 20, 28],
        ],
        lines,
        { station: 1, t: 21 },
      ),
      // The wait's ends count.
      covers([[1, 15, 35]], lines, { station: 1, t: 20 }),
      covers([[1, 15, 35]], lines, { station: 1, t: 30 }),
    ]).toEqual([true, false, true, false, false, false, false, true, false, true, true, true]);
  });
});
