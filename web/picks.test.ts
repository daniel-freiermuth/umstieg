import { describe, expect, test } from "bun:test";
import { type Picked, narrow, pinAfter, step, toggle } from "./picks";
import type { Segment } from "./reach";

const pick = (id: string, chains: number[], segments: Segment[] | null = null): Picked => ({
  id,
  info: id,
  chains,
  reach: segments === null ? null : { key: `r:${id}`, segments },
});
const all = (): boolean => true;
const sorted = (segments: readonly Segment[]): Segment[] =>
  [...segments].sort((a, b) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2]);

describe("narrow", () => {
  test("nothing picked lists nothing", () => {
    expect(narrow([], all)).toBeNull();
  });

  test("lists the connections passing every pick, in the first pick's order", () => {
    const c = narrow([pick("ICE 378", [5, 1, 3, 2]), pick("Ludwigshafen", [2, 3, 9])], all);
    expect(c?.chains).toEqual([3, 2]);
    expect(c?.info).toBe("ICE 378 + Ludwigshafen");
  });

  test("hidden connections are not listed", () => {
    expect(narrow([pick("a", [1, 2, 3]), pick("b", [3, 2])], (i) => i !== 2)?.chains).toEqual([3]);
  });

  test("the highlight is what every highlighted pick reaches, whatever the order", () => {
    const a = pick(
      "a",
      [1],
      [
        [0, 0, 10],
        [1, 5, 20],
      ],
    );
    const b = pick(
      "b",
      [1],
      [
        [0, 5, 15],
        [2, 0, 9],
      ],
    );
    const plain = pick("c", [1]);
    const ab = narrow([a, plain, b], all)?.reach;
    const ba = narrow([b, a, plain], all)?.reach;
    expect(sorted(ab?.segments ?? [])).toEqual([[0, 5, 10]]);
    expect(sorted(ba?.segments ?? [])).toEqual([[0, 5, 10]]);
    // The key changes with the picks, so the highlight is redrawn.
    expect(ab?.key).toBe("r:a|c|r:b");
  });

  test("picks without a highlight have none together", () => {
    expect(narrow([pick("a", [1]), pick("b", [1])], all)?.reach).toBeNull();
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
    const a = pick("a", [1]),
      b = pick("b", [2]);
    const picks = toggle(toggle([], a), b);
    expect(picks.map((p) => p.id)).toEqual(["a", "b"]);
    // The same line clicked again is a new object with the same id.
    expect(toggle(picks, pick("a", [1])).map((p) => p.id)).toEqual(["b"]);
    expect(picks.map((p) => p.id)).toEqual(["a", "b"]);
  });
});
