import { describe, expect, test } from "bun:test";
import {
  type Connection,
  type Limits,
  type Need,
  type Step,
  type Stretch,
  ENOUGH_BUFFER,
  bestThrough,
  callsOf,
  isDetour,
  meets,
} from "./best";

// Times are minutes, in the data ms.
const M = 60e3;
/** A train: [station, minute] boards or alights, [station, arrival, departure] stops between. */
function ride(trip: number, calls: ([number, number] | [number, number, number])[]): Stretch {
  return { kind: "ride", trip, raw: calls.flatMap(([s, ...ts]) => ts.map((t): [number, number] => [t * M, s])) };
}
/** A walk as the network has it: walked at some time (`at`), `minutes` long. */
const walk = (a: number, b: number, minutes: number, at = 0): Stretch => ({
  kind: "walk",
  trip: null,
  raw: [
    [at * M, a],
    [(at + minutes) * M, b],
  ],
});
const limits = (over: Partial<Limits> = {}): Limits => ({
  origin: 0,
  destination: 9,
  from: 0,
  until: 60 * M,
  maxDuration: 600 * M,
  maxChanges: 5,
  missedBy: 5 * M,
  minTransfer: 3 * M,
  keepDetours: false,
  ...over,
});
/** A connection as "trip:from>to@dep … walk a>b", for comparing. */
function show(c: Connection): string {
  return c.steps
    .map((s) => {
      if (s.kind === "walk") return `walk ${String(s.from)}>${String(s.to)}@${String(s.start / M)}`;
      const a = s.calls[0],
        b = s.calls[s.calls.length - 1];
      return `${String(s.trip)}:${String(a?.station)}>${String(b?.station)}@${String((a?.dep ?? 0) / M)}`;
    })
    .join(" ");
}
const search = (pieces: Stretch[], needs: Need[] = [], over: Partial<Limits> = {}): string[] =>
  bestThrough(
    pieces,
    pieces.map((_, i) => i),
    10,
    limits(over),
    needs,
  ).found.map(show);

describe("callsOf", () => {
  test("a run's points become stops: departure only first, arrival only last", () => {
    expect(
      callsOf([
        [0, 1],
        [10, 2],
        [12, 2],
        [20, 3],
      ]),
    ).toEqual([
      { station: 1, arr: null, dep: 0 },
      { station: 2, arr: 10, dep: 12 },
      { station: 3, arr: 20, dep: null },
    ]);
    expect(callsOf([])).toEqual([]);
  });
});

describe("the best connections", () => {
  // From 0 to 9: train 1 leaves at 10 via 1 (change, 10 min) to train 2; train 3
  // leaves at 20 via 2 (change, 14 min) to the same train 2.
  const net = [
    ride(1, [
      [0, 10],
      [1, 30],
    ]),
    ride(2, [
      [1, 40, 40],
      [2, 50, 50],
      [9, 70],
    ]),
    ride(3, [
      [0, 20],
      [2, 36],
    ]),
  ];

  test("without picks: none beaten", () => {
    // Train 3 leaves later and changes with more buffer: train 1 is beaten.
    expect(search(net)).toEqual(["3:0>2@20 2:2>9@50"]);
  });

  test("through a pick, the best of those passing it, beaten elsewhere or not", () => {
    const onTrain1: Need = { kind: "ride", trip: 1, from: 0, at: 10 * M, to: 1, until: 30 * M };
    expect(search(net, [onTrain1])).toEqual(["1:0>1@10 2:1>9@40"]);
    // At station 1 at 35 (waiting for train 2) or anywhere at 1: the same.
    expect(search(net, [{ kind: "stay", station: 1, at: 35 * M }])).toEqual(["1:0>1@10 2:1>9@40"]);
    expect(search(net, [{ kind: "station", station: 1 }])).toEqual(["1:0>1@10 2:1>9@40"]);
    // In train 2 standing at 2 (at 50), both pass; train 3's still beats.
    expect(search(net, [{ kind: "stay", station: 2, at: 50 * M }])).toEqual(["3:0>2@20 2:2>9@50"]);
    // Picks no connection passes together.
    const onTrain3: Need = { kind: "ride", trip: 3, from: 0, at: 20 * M, to: 2, until: 36 * M };
    expect(search(net, [onTrain1, onTrain3])).toEqual([]);
  });

  test("the window, the travel time and the changes limit", () => {
    expect(search(net, [], { from: 15 * M })).toEqual(["3:0>2@20 2:2>9@50"]);
    expect(search(net, [], { until: 15 * M })).toEqual(["1:0>1@10 2:1>9@40"]);
    expect(search(net, [], { maxDuration: 49 * M })).toEqual([]);
    expect(search(net, [], { maxDuration: 50 * M })).toEqual(["3:0>2@20 2:2>9@50"]);
    expect(search(net, [], { maxChanges: 0 })).toEqual([]);
  });

  test("latest departure first, then earliest arrival; changes counted", () => {
    // Train 21 at 5 direct to 9 by 60; train 22 at 5 by 50 but with a change at 1.
    const pieces = [
      ride(21, [
        [0, 5],
        [9, 60],
      ]),
      ride(22, [
        [0, 5],
        [1, 20],
      ]),
      ride(23, [
        [1, 25],
        [9, 50],
      ]),
      ride(24, [
        [0, 30],
        [9, 90],
      ]),
    ];
    const found = bestThrough(pieces, [0, 1, 2, 3], 10, limits(), []).found;
    expect(found.map((c) => [show(c), c.changes])).toEqual([
      ["24:0>9@30", 0],
      ["22:0>1@5 23:1>9@25", 1],
      ["21:0>9@5", 0],
    ]);
  });

  test("no riding on where merged data has a train arrive before it left", () => {
    const back = [
      ride(25, [
        [0, 30],
        [1, 20],
        [9, 40],
      ]),
    ];
    expect(search(back)).toEqual([]);
    // Arriving the minute it left (squeezed live times) is fine.
    const squeezed = [
      ride(26, [
        [0, 30],
        [9, 30],
      ]),
    ];
    expect(search(squeezed)).toEqual(["26:0>9@30"]);
  });

  test("a change missed by a little counts, with its buffer negative", () => {
    // Train 2 leaves 1 at 40; train 4 arrives there at 43.
    const late = [
      ...net,
      ride(4, [
        [0, 25],
        [1, 43],
      ]),
    ];
    const found = bestThrough(late, [0, 1, 2, 3], 10, limits(), [
      { kind: "ride", trip: 4, from: 0, at: 25 * M, to: 1, until: 43 * M },
    ]).found;
    expect(found.map((c) => [show(c), c.slack === null ? null : c.slack / M])).toEqual([["4:0>1@25 2:1>9@40", -3]]);
    expect(
      search(late, [{ kind: "ride", trip: 4, from: 0, at: 25 * M, to: 1, until: 43 * M }], { missedBy: 2 * M }),
    ).toEqual([]);
  });

  test("walks: from the origin before the first train, after a train, to the destination", () => {
    const pieces = [
      walk(0, 5, 4, 26),
      ride(7, [
        [5, 30],
        [6, 50],
      ]),
      walk(6, 9, 3, 50),
      // A walk on from a walk is no connection, nor walking all the way.
      walk(5, 8, 1, 30),
      walk(0, 9, 30, 10),
    ];
    const [c, ...more] = bestThrough(pieces, [0, 1, 2, 3, 4], 10, limits({ from: 20 * M }), []).found;
    expect(more).toEqual([]);
    expect(c && show(c)).toBe("walk 0>5@26 7:5>6@30 walk 6>9@50");
    expect([c?.departure, c?.arrival, c?.changes].map((t) => (t ?? 0) / M)).toEqual([26, 53, 0]);
    expect(c?.steps.map((s) => (s.kind === "walk" ? [s.start / M, s.end / M] : null))).toEqual([
      [26, 30],
      null,
      [50, 53],
    ]);
    // Leaving the origin before the window isn't allowed either.
    expect(search(pieces, [], { from: 27 * M })).toEqual([]);
  });

  test("no station twice", () => {
    // Train 8 to 2, train 9 on to 3, train 10 back via 2 to 9: passing train 9
    // means being at 2 twice. Train 8 then 10 from 2 is fine.
    const pieces = [
      ride(8, [
        [0, 10],
        [2, 20],
      ]),
      ride(9, [
        [2, 25],
        [3, 30],
      ]),
      ride(10, [
        [3, 35],
        [2, 40, 40],
        [9, 50],
      ]),
    ];
    expect(search(pieces)).toEqual(["8:0>2@10 10:2>9@40"]);
    expect(search(pieces, [{ kind: "ride", trip: 9, from: 2, at: 25 * M, to: 3, until: 30 * M }])).toEqual([]);
  });

  test("ties are all kept; buffers beyond enough count the same", () => {
    // Two ways to the same train 2 at 1 with the same times: 11 via 3, 12 via 4.
    const pieces = [
      ride(11, [
        [0, 10],
        [3, 15],
        [1, 20],
      ]),
      ride(12, [
        [0, 10],
        [4, 15],
        [1, 20],
      ]),
      ride(2, [
        [1, 20 + ENOUGH_BUFFER / M + 30],
        [9, 300],
      ]),
      ride(13, [
        [0, 9],
        [1, 19],
      ]),
    ];
    // 13 leaves earlier for a buffer that is enough either way: beaten.
    expect(search(pieces).sort()).toEqual(["11:0>1@10 2:1>9@140", "12:0>1@10 2:1>9@140"]);
  });

  test("no detours: a train is boarded where it is first catchable comfortably", () => {
    // Freiburg (0) → Augsburg (9): ICE 374 at 55 via Offenburg (1) to Mannheim (3);
    // ICE 1067 at 81 via Offenburg to Stuttgart (2); ICE 593 from Mannheim via Stuttgart.
    const pieces = [
      ride(374, [
        [0, 55],
        [1, 85, 87],
        [3, 142],
      ]),
      ride(1067, [
        [0, 81],
        [1, 115, 117],
        [2, 183],
      ]),
      ride(593, [
        [3, 153],
        [2, 191, 197],
        [9, 281],
      ]),
    ];
    const on374: Need = { kind: "ride", trip: 374, from: 0, at: 55 * M, to: 1, until: 85 * M };
    // ICE 374 to Offenburg for ICE 1067, which left Freiburg after it: a detour.
    expect(search(pieces, [on374], { until: 120 * M })).toEqual(["374:0>3@55 593:3>9@153"]);
    // Known from ICE 1067's other piece: the order of the network's pieces doesn't matter.
    const split = [
      pieces[0],
      pieces[2],
      ride(1067, [
        [1, 117],
        [2, 183],
      ]),
      ride(1067, [
        [0, 81],
        [1, 115],
      ]),
    ].flatMap((p) => (p ? [p] : []));
    expect(search(split, [on374], { until: 120 * M })).toEqual(["374:0>3@55 593:3>9@153"]);
    expect(search(pieces, [on374], { until: 120 * M, keepDetours: true }).sort()).toEqual([
      "374:0>1@55 1067:1>2@117 593:2>9@197",
      "374:0>3@55 593:3>9@153",
    ]);
  });

  test("a detour raising the worst buffer stays", () => {
    // At 1 from 30: train 5 leaves at 31 (a 1-minute change, below the 3-minute
    // minimum) on to 2 and 9; train 6 takes 1 → 3 → 2 and changes there with 5 min.
    const pieces = [
      ride(4, [
        [0, 10],
        [1, 30],
      ]),
      ride(5, [
        [1, 31],
        [2, 40, 40],
        [9, 60],
      ]),
      ride(6, [
        [1, 32],
        [3, 34],
      ]),
      ride(7, [
        [3, 34],
        [2, 35],
      ]),
    ];
    const via3: Need = { kind: "station", station: 3 };
    expect(search(pieces, [via3])).toEqual(["4:0>1@10 6:1>3@32 7:3>2@34 5:2>9@40"]);
    // With 5 leaving 1 at 34 (4 minutes), going round for it is a detour.
    const comfortable = pieces.map((p) =>
      p.trip === 5
        ? ride(5, [
            [1, 34],
            [2, 40, 40],
            [9, 60],
          ])
        : p,
    );
    expect(search(comfortable, [via3])).toEqual([]);
  });

  test("a search cut short says so", () => {
    expect(bestThrough(net, [0, 1, 2], 10, limits(), [], 2).complete).toBe(false);
    expect(bestThrough(net, [0, 1, 2], 10, limits(), []).complete).toBe(true);
  });
});

describe("isDetour", () => {
  const at = (
    station: number,
    arrival: number | null,
    since: number,
    slackOut: number | null = null,
  ): { station: number; arrival: number | null; since: number; slackOut: number | null } => ({
    station,
    arrival,
    since,
    slackOut,
  });
  const calls = (...stops: [number, number][]): ReturnType<typeof callsOf> =>
    stops.map(([station, dep]) => ({ station, arr: dep, dep }));
  test("nothing to compare at the start", () => {
    expect(isDetour([], calls([0, 5]), 10, 3)).toBe(false);
    expect(isDetour([at(0, null, 5)], calls([0, 5]), 10, 3)).toBe(false);
  });
  test("at the origin any later departure of the train counts", () => {
    // At 0 since 5 (the first train), now at 1 (arrived 20) boarding at 30 a train that left 0 at 8.
    expect(isDetour([at(0, null, 5, 10), at(1, 20, 20)], calls([0, 8]), 30, 3)).toBe(true);
    expect(isDetour([at(0, null, 9, 10), at(1, 20, 20)], calls([0, 8]), 30, 3)).toBe(false);
    // Leaving as the connection does: the same train, so boarding it later is a detour.
    expect(isDetour([at(0, null, 8, 10), at(1, 20, 20)], calls([0, 8]), 30, 3)).toBe(true);
  });
  test("elsewhere only if catchable comfortably, with at least the detour's worst buffer", () => {
    // At 2 from 40 (left again 9 minutes later), now at 3 (arrived 50), boarding at 52: the
    // detour's worst buffer is 2. The train left 2 at 45 (5 minutes there) or 43 (3) or 42 (2).
    const stops = [at(0, null, 0, 0), at(2, 40, 40, 9), at(3, 50, 50)];
    expect([45, 43, 42, 39].map((d) => isDetour(stops, calls([2, d]), 52, 3))).toEqual([true, true, false, false]);
    // Boarding at 57 here (7 minutes), 6 at 2 before: at 2 it must have been at least 6.
    const roomy = [at(0, null, 0, 0), at(2, 40, 40, 6), at(3, 50, 50)];
    expect([46, 45].map((d) => isDetour(roomy, calls([2, d]), 57, 3))).toEqual([true, false]);
    // Every change since counts: a 1-minute one at 3 on the way from 2 to 4.
    const tight = [at(0, null, 0, 0), at(2, 40, 40, 9), at(3, 50, 50, 1), at(4, 60, 60)];
    expect(isDetour(tight, calls([2, 45]), 70, 3)).toBe(true);
  });
});

describe("meets", () => {
  const steps: Step[] = [
    { kind: "walk", from: 0, to: 5, start: 0, end: 4 },
    {
      kind: "ride",
      trip: 7,
      calls: [
        { station: 5, arr: null, dep: 10 },
        { station: 6, arr: 20, dep: 22 },
        { station: 7, arr: 30, dep: null },
      ],
    },
  ];
  test("each kind of pick", () => {
    expect([
      meets(steps, { kind: "ride", trip: 7, from: 6, at: 22, to: 7, until: 30 }),
      meets(steps, { kind: "ride", trip: 7, from: 6, at: 22, to: 7, until: 31 }),
      meets(steps, { kind: "ride", trip: 8, from: 6, at: 22, to: 7, until: 30 }),
      meets(steps, { kind: "ride", trip: 7, from: 5, at: 22, to: 7, until: 30 }),
      meets(steps, { kind: "ride", trip: 7, from: 6, at: 21, to: 7, until: 30 }),
      meets(steps, { kind: "walk", from: 0, to: 6, at: 0 }),
      meets(steps, { kind: "walk", from: 1, to: 5, at: 0 }),
      meets(steps, { kind: "station", station: 0 }),
      meets(steps, { kind: "walk", from: 0, to: 5, at: 0 }),
      meets(steps, { kind: "walk", from: 0, to: 5, at: 1 }),
      meets(steps, { kind: "station", station: 6 }),
      meets(steps, { kind: "station", station: 5 }),
      meets(steps, { kind: "station", station: 8 }),
      // Waiting for the train after the walk, standing at 6.
      meets(steps, { kind: "stay", station: 5, at: 7 }),
      meets(steps, { kind: "stay", station: 6, at: 21 }),
      meets(steps, { kind: "stay", station: 6, at: 23 }),
      meets(steps, { kind: "stay", station: 7, at: 30 }),
      meets(steps, { kind: "stay", station: 5, at: 4 }),
      meets(steps, { kind: "stay", station: 5, at: 3 }),
      meets(steps, { kind: "stay", station: 6, at: 7 }),
    ]).toEqual([
      true,
      false,
      false,
      false,
      false,
      false,
      false,
      true,
      true,
      false,
      true,
      true,
      false,
      true,
      true,
      false,
      false,
      true,
      false,
      false,
    ]);
    // Left where the train stands on: not staying there while it does.
    const off: Step[] = [
      {
        kind: "ride",
        trip: 7,
        calls: [
          { station: 5, arr: null, dep: 10 },
          { station: 6, arr: 20, dep: 22 },
        ],
      },
      { kind: "walk", from: 6, to: 8, start: 20, end: 24 },
    ];
    expect([
      meets(off, { kind: "stay", station: 6, at: 20 }),
      meets(off, { kind: "stay", station: 6, at: 21 }),
      // Walked to only.
      meets(off, { kind: "station", station: 8 }),
    ]).toEqual([true, false, true]);
  });
});

// ---------------------------------------------------------------- reference

function random(seed: number): (n: number) => number {
  let x = seed;
  return (n) => {
    x = (x * 1103515245 + 12345) % 2147483648;
    return Math.floor((x / 2147483648) * n);
  };
}

/** Trains and walks among stations 0 (origin) to 6 (destination 9 is renamed 6). */
function randomNet(rnd: (n: number) => number): Stretch[] {
  const pieces: Stretch[] = [];
  for (let trip = 0; trip < 6 + rnd(10); trip++) {
    const stops = 2 + rnd(4);
    let t = rnd(60);
    const calls: [number, number, number][] = [];
    let s = rnd(4);
    for (let k = 0; k < stops; k++) {
      // Mostly on towards 6, now and then back.
      s = Math.max(0, Math.min(6, rnd(5) === 0 ? s - 1 - rnd(2) : s + 1 + rnd(2)));
      if (calls.some((c) => c[0] === s)) continue;
      const stand = rnd(3);
      calls.push([s, t, t + stand]);
      t += stand + 1 + rnd(20);
    }
    if (calls.length < 2) continue;
    pieces.push(
      ride(
        trip,
        calls.map(([s, a, d], k): [number, number] | [number, number, number] =>
          k === 0 ? [s, d] : k === calls.length - 1 ? [s, a] : [s, a, d],
        ),
      ),
    );
  }
  // Footpaths: one length per pair of stations.
  const paths = new Set<string>();
  for (let w = 0; w < rnd(4); w++) {
    const a = rnd(7),
      b = rnd(7);
    if (a !== b && !paths.has(`${String(a)}>${String(b)}`)) pieces.push(walk(a, b, 1 + rnd(6)));
    paths.add(`${String(a)}>${String(b)}`);
  }
  return pieces;
}

/** Every connection by the rules, built without pruning and checked as a whole. */
function everyConnection(pieces: Stretch[], l: Limits): Step[][] {
  const runs = pieces.flatMap((p) =>
    p.kind === "ride" && p.trip !== null ? [{ trip: p.trip, calls: callsOf(p.raw) }] : [],
  );
  const walks = pieces.flatMap((p) => {
    const a = p.raw[0],
      b = p.raw[p.raw.length - 1];
    return p.kind === "walk" && a && b ? [{ from: a[1], to: b[1], d: b[0] - a[0] }] : [];
  });
  const out: Step[][] = [];
  const grow = (path: Step[], at: number, time: number): void => {
    if (at === l.destination && path.some((s) => s.kind === "ride")) out.push(path);
    if (at === l.destination || path.length > 6) return;
    const last = path[path.length - 1];
    if (last?.kind !== "walk")
      for (const w of walks.filter((w) => w.from === at))
        // Before the first train the time is open: the walk's own (0 to its length).
        grow(
          [...path, { kind: "walk", from: at, to: w.to, start: Math.max(time, 0), end: Math.max(time, 0) + w.d }],
          w.to,
          time + w.d,
        );
    for (const run of runs)
      run.calls.forEach((b, i) => {
        if (b.station !== at || b.dep === null || (path.length > 0 && b.dep < time - l.missedBy)) return;
        for (let j = i + 1; j < run.calls.length; j++) {
          const a = run.calls[j];
          if (a === undefined || a.arr === null) break;
          grow([...path, { kind: "ride", trip: run.trip, calls: run.calls.slice(i, j + 1) }], a.station, a.arr);
        }
      });
  };
  grow([], l.origin, -Infinity);
  return out.flatMap((path) => {
    // A walk to the first train starts when the connection leaves.
    const firstRide = path.findIndex((s) => s.kind === "ride");
    const ride = path[firstRide];
    if (ride?.kind !== "ride") return [];
    const dep = ride.calls[0]?.dep ?? 0;
    const lead = path.slice(0, firstRide).reduce((d, s) => d + (s.kind === "walk" ? s.end - s.start : 0), 0);
    const fixed = path.map((s, i) =>
      i < firstRide && s.kind === "walk" ? { ...s, start: dep - lead, end: dep - lead + s.end - s.start } : s,
    );
    return valid(fixed, l) && (l.keepDetours || !detour(fixed, pieces, l)) ? [fixed] : [];
  });
}

/** The server's detour rule over a whole connection, its presences built from the path. */
function detour(path: Step[], pieces: Stretch[], l: Limits): boolean {
  const stops = new Map<number, ReturnType<typeof callsOf>>();
  for (const p of pieces)
    if (p.kind === "ride" && p.trip !== null) stops.set(p.trip, [...(stops.get(p.trip) ?? []), ...callsOf(p.raw)]);
  const at: Parameters<typeof isDetour>[0][number][] = [];
  let rides = 0;
  for (const s of path) {
    if (s.kind === "walk") {
      const d = s.end - s.start;
      if (rides === 0)
        at.push(
          { station: l.origin, arrival: null, since: l.from, slackOut: null },
          { station: s.to, arrival: l.from + d, since: l.from + d, slackOut: null },
        );
      else at.push({ station: s.to, arrival: s.end, since: s.end, slackOut: null });
      continue;
    }
    const board = s.calls[0],
      alight = s.calls[s.calls.length - 1];
    const dep = board?.dep ?? null,
      arr = alight?.arr ?? null;
    if (!board || !alight || dep === null || arr === null) return true;
    const earlier = (stops.get(s.trip) ?? []).filter((c) => (c.dep ?? c.arr ?? Infinity) < dep);
    if (isDetour(at, earlier, dep, l.minTransfer)) return true;
    const here = at[at.length - 1];
    if (rides > 0 && here) here.slackOut = here.arrival === null ? null : dep - here.arrival;
    else at.push({ station: board.station, arrival: null, since: dep, slackOut: null });
    at.push({ station: alight.station, arrival: arr, since: arr, slackOut: null });
    rides++;
  }
  return false;
}

function valid(path: Step[], l: Limits): boolean {
  const rides = path.filter((s) => s.kind === "ride");
  const first = rides[0];
  if (!first) return false;
  const lead = path.slice(0, path.indexOf(first)).reduce((d, s) => d + (s.kind === "walk" ? s.end - s.start : 0), 0);
  const leave = (first.calls[0]?.dep ?? 0) - lead;
  const lastStep = path[path.length - 1];
  const arrival = lastStep?.kind === "walk" ? lastStep.end : (lastStep?.calls[lastStep.calls.length - 1]?.arr ?? 0);
  // Walks: never two in a row; the one before the first train not to the destination.
  if (path.some((s, i) => s.kind === "walk" && path[i + 1]?.kind === "walk")) return false;
  if (path[0]?.kind === "walk" && path[0].to === l.destination) return false;
  const stations = [
    l.origin,
    ...path.flatMap((s) => (s.kind === "walk" ? [s.to] : s.calls.slice(1).map((c) => c.station))),
  ];
  return (
    leave >= l.from &&
    leave <= l.until &&
    arrival - leave <= l.maxDuration &&
    rides.length - 1 <= l.maxChanges &&
    new Set(stations).size === stations.length &&
    new Set(rides.map((r) => r.trip)).size === rides.length &&
    slackOf(path) >= -l.missedBy
  );
}

interface Key {
  dep: number;
  arr: number;
  changes: number;
  slack: number;
}

function slackOf(path: Step[]): number {
  let worst = Infinity,
    ready: number | null = null;
  for (const s of path) {
    if (s.kind === "walk") {
      if (ready !== null) ready += s.end - s.start;
      continue;
    }
    if (ready !== null) worst = Math.min(worst, (s.calls[0]?.dep ?? 0) - ready);
    ready = s.calls[s.calls.length - 1]?.arr ?? 0;
  }
  return worst;
}

describe("on random networks", () => {
  test("the same as building every connection and keeping the best passing the picks", () => {
    const l = limits({ destination: 6, until: 40 * M, maxDuration: 70 * M, maxChanges: 3, missedBy: 3 * M });
    let withPicks = 0;
    for (let seed = 1; seed <= 3000; seed++) {
      const rnd = random(seed);
      const pieces = randomNet(rnd);
      const all = everyConnection(pieces, l);
      // Picks from a random connection, so that some pass them.
      const some = all[rnd(Math.max(all.length, 1))];
      const needs: Need[] = [];
      for (const s of some ?? []) {
        if (rnd(3) > 0) continue;
        if (s.kind === "walk") needs.push({ kind: "walk", from: s.from, to: s.to, at: s.start });
        else {
          const [a, b] = s.calls;
          if (a && b && a.dep !== null && b.arr !== null)
            needs.push({ kind: "ride", trip: s.trip, from: a.station, at: a.dep, to: b.station, until: b.arr });
          if (rnd(2) === 0 && b) needs.push({ kind: "station", station: b.station });
          const [stand, leave] = [b?.arr ?? null, b?.dep ?? null];
          if (b && stand !== null && leave !== null) needs.push({ kind: "stay", station: b.station, at: stand });
        }
      }
      if (needs.length && all.length > 1) withPicks++;
      const passing = all.filter((p) => needs.every((n) => meets(p, n)));
      const key = (p: Step[]): Key => {
        const firstRide = p.findIndex((s) => s.kind === "ride");
        const r0 = p[firstRide];
        const lead = p.slice(0, firstRide).reduce((d, s) => d + (s.kind === "walk" ? s.end - s.start : 0), 0);
        const last = p[p.length - 1];
        const rides = p.filter((s) => s.kind === "ride").length;
        return {
          dep: (r0?.kind === "ride" ? (r0.calls[0]?.dep ?? 0) : 0) - lead,
          arr: last?.kind === "walk" ? last.end : (last?.calls[last.calls.length - 1]?.arr ?? 0),
          changes: rides - 1,
          slack: rides > 1 ? Math.min(slackOf(p), ENOUGH_BUFFER) : ENOUGH_BUFFER,
        };
      };
      const beaten = (a: Key, b: Key): boolean =>
        JSON.stringify(a) !== JSON.stringify(b) &&
        a.dep >= b.dep &&
        a.arr <= b.arr &&
        a.changes <= b.changes &&
        a.slack >= b.slack;
      const expected = passing
        .filter((p) => !passing.some((q) => beaten(key(q), key(p))))
        .map((steps) => show({ steps, departure: 0, arrival: 0, changes: 0, slack: null }))
        .sort();
      const got = bestThrough(
        pieces,
        pieces.map((_, i) => i),
        7,
        l,
        needs,
      );
      expect(got.complete).toBe(true);
      expect([seed, got.found.map(show).sort()]).toEqual([seed, [...new Set(expected)]]);
    }
    expect(withPicks).toBeGreaterThan(400);
  });
});
