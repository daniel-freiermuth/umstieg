import { describe, expect, test } from "bun:test";
import { type Picked, atHeights, narrow, pinAfter, step, toggle } from "./picks";
import type { Segment } from "./reach";

const pick = (id: string, station: number | null, segments: Segment[] | null = null): Picked => ({
  id,
  info: id,
  pts: [],
  raw: [],
  need: station === null ? null : { kind: "station", station },
  reach: segments === null ? null : { key: `r:${id}`, segments },
});
const sorted = (segments: readonly Segment[]): Segment[] =>
  [...segments].sort((a, b) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2]);

describe("narrow", () => {
  test("nothing picked lists nothing", () => {
    expect(narrow([])).toBeNull();
  });

  test("connections must pass every pick that asks something of them", () => {
    const c = narrow([pick("Mannheim", 5), pick("anything", null), pick("Ludwigshafen", 2)]);
    expect(c?.needs).toEqual([
      { kind: "station", station: 5 },
      { kind: "station", station: 2 },
    ]);
    expect(c?.info).toBe("Mannheim + anything + Ludwigshafen");
  });

  test("the highlight is what every highlighted pick reaches, whatever the order", () => {
    const a = pick("a", null, [
      [0, 0, 10],
      [1, 5, 20],
    ]);
    const b = pick("b", null, [
      [0, 5, 15],
      [2, 0, 9],
    ]);
    const plain = pick("c", null);
    const ab = narrow([a, plain, b])?.reach;
    const ba = narrow([b, a, plain])?.reach;
    expect(sorted(ab?.segments ?? [])).toEqual([[0, 5, 10]]);
    expect(sorted(ba?.segments ?? [])).toEqual([[0, 5, 10]]);
    // The key changes with the picks, so the highlight is redrawn.
    expect(ab?.key).toBe("r:a|c|r:b");
  });

  test("picks without a highlight have none together", () => {
    expect(narrow([pick("a", 1), pick("b", 1)])?.reach).toBeNull();
  });
});

describe("pinAfter", () => {
  const cases: [string, number[], number | null, number | null][] = [
    ["one left is selected", [4], null, 4],
    ["one left replaces the old selection", [4], 7, 4],
    ["the selection stays while listed", [3, 7, 9], 7, 7],
    ["the selection goes when no longer listed", [3, 9], 7, null],
    ["nothing selected stays so", [3, 9], null, null],
    ["an empty list selects nothing", [], 7, null],
  ];
  for (const [name, chains, pinned, expected] of cases)
    test(name, () => {
      expect(pinAfter(chains, pinned)).toBe(expected);
    });
});

describe("step", () => {
  const list = [10, 20, 30];
  const cases: [string, number | null, boolean, number | null][] = [
    ["down from nothing: the first", null, true, 10],
    ["up from nothing: the first", null, false, 10],
    ["down", 10, true, 20],
    ["up", 20, false, 10],
    ["down from the last wraps", 30, true, 10],
    ["up from the first wraps", 10, false, 30],
    ["from one not listed: the first", 99, false, 10],
  ];
  for (const [name, pinned, down, expected] of cases)
    test(name, () => {
      expect(step(list, pinned, down)).toBe(expected);
    });

  test("an empty list has nothing to step to", () => {
    expect(step([], null, true)).toBeNull();
    expect(step([], 5, false)).toBeNull();
  });
});

describe("toggle", () => {
  test("adds a new pick at the end and removes a picked one by id", () => {
    const a = pick("a", 1),
      b = pick("b", 2);
    const picks = toggle(toggle([], a), b);
    expect(picks.map((p) => p.id)).toEqual(["a", "b"]);
    // The same line clicked again is a new object with the same id.
    expect(toggle(picks, pick("a", 1)).map((p) => p.id)).toEqual(["b"]);
    expect(picks.map((p) => p.id)).toEqual(["a", "b"]);
  });
});

describe("atHeights", () => {
  // A piece's calls as (time, station), whatever the metric.
  const piece = (...raw: [number, number][]): { raw: [number, number][] } => ({ raw });
  // A pick of a stretch of piece 0, from its first to its last call.
  const stretch = (...raw: [number, number][]): Picked => ({
    ...pick("ride", null),
    raw,
    spot: { piece: 0, from: raw[0]?.[0] ?? 0, to: raw[raw.length - 1]?.[0] ?? 0 },
  });
  // Where a marker is drawn: time and height.
  const drawn = (p: Picked | undefined): number[][] => (p?.pts ?? []).map(([t, y]) => [t, y]);

  test("a picked stretch follows its line in the new metric, through stops that only now have a height", () => {
    // A (station 0) at 0 → X (1) at 5 → C (2) at 10, picked where X had no
    // height: the line, and the pick, went straight A → C.
    const ride: Picked = {
      ...stretch([0, 0], [10, 2]),
      pts: [
        [0, 120, 0],
        [10, 0, 2],
      ],
      need: { kind: "ride", trip: 4, from: 0, at: 0, to: 2, until: 10 },
      reach: { key: "r", segments: [[0, 0, 10]] },
    };
    const [r] = atHeights([ride], [piece([0, 0], [5, 1], [10, 2])], [10, 50, 0]);
    expect(drawn(r)).toEqual([
      [0, 10],
      [5, 50],
      [10, 0],
    ]);
    expect({ ...r, pts: ride.pts }).toEqual(ride);
    expect(ride.pts[0]).toEqual([0, 120, 0]);
  });

  test("a picked stretch ending at a stop the new metric has no height for stays on the line past it", () => {
    const ride = stretch([0, 0], [5, 1]);
    expect(drawn(atHeights([ride], [piece([0, 0], [5, 1], [10, 2])], [10, null, 0])[0])).toEqual([
      [0, 10],
      [5, 5],
    ]);
  });

  test("a picked stretch keeps to its stops where the next or last hop takes no time", () => {
    // A at 0 → B at 10 → C at 10 → D at 20: B → C is a hop of its own.
    const line = piece([0, 0], [10, 1], [10, 2], [20, 3]);
    const heights = [30, 20, 10, 0];
    const [ab, bc, cd] = atHeights(
      [stretch([0, 0], [10, 1]), stretch([10, 1], [10, 2]), stretch([10, 2], [20, 3])],
      [line],
      heights,
    );
    expect(drawn(ab)).toEqual([
      [0, 30],
      [10, 20],
    ]);
    expect(drawn(bc)).toEqual([
      [10, 20],
      [10, 10],
    ]);
    expect(drawn(cd)).toEqual([
      [10, 10],
      [20, 0],
    ]);
  });

  // Picked as station 1's row, and as a stay at station 2.
  const station: Picked = {
    ...pick("station", 1),
    pts: [
      [0, 90, 1],
      [20, 90, 1],
    ],
    raw: [
      [0, 1],
      [20, 1],
    ],
  };
  const stay: Picked = {
    ...pick("stay", null),
    pts: [
      [5, 40, 2],
      [15, 40, 2],
    ],
    raw: [
      [5, 2],
      [15, 2],
    ],
    spot: { station: 2, t: 8 },
  };

  test("a station's row or a stay moves to the station's height, and is left out without one, as its line is", () => {
    const [s, w] = atHeights([station, stay], [], [null, 250, null]);
    expect(s?.pts).toEqual([
      [0, 250, 1],
      [20, 250, 1],
    ]);
    expect(w?.pts).toEqual([]);
  });

  test("a station's row or a stay is back on its line after a metric that left its station out", () => {
    const [s, w] = atHeights(atHeights([station, stay], [], [null, null, null]), [], [null, 250, 30]);
    expect(s?.pts).toEqual([
      [0, 250, 1],
      [20, 250, 1],
    ]);
    expect(w?.pts).toEqual([
      [5, 30, 2],
      [15, 30, 2],
    ]);
  });
});
