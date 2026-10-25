import { describe, expect, test } from "bun:test";
import { type Data, applyHeights, decodeData, extent, load } from "./data";
import { type Json, parseJson } from "./decode";
import { network } from "./reach";

// One connection of a real plot (Mannheim Hbf → Frankfurt Hbf, src/plot.rs
// output) with two of its fallbacks.
const text = await Bun.file(new URL("../testdata/web/plot.json", import.meta.url)).text();
const json = parseJson(text);
const fresh = (): Data => decodeData(json, "data");
const MIN = 60e3;

/** The fixture with one value replaced at a path of keys and indices. */
function patched(path: (string | number)[], value: Json): Json {
  const go = (v: Json, rest: (string | number)[]): Json => {
    const [key, ...more] = rest;
    if (key === undefined) return value;
    if (Array.isArray(v) && typeof key === "number") return v.map((x: Json, i) => (i === key ? go(x, more) : x));
    if (typeof v === "object" && v !== null && !Array.isArray(v) && typeof key === "string")
      return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, k === key ? go(x, more) : x]));
    throw new Error(`no ${String(key)}`);
  };
  return go(json, path);
}
function without(key: string, from: (string | number)[]): Json {
  const go = (v: Json, rest: (string | number)[]): Json => {
    const [k, ...more] = rest;
    if (Array.isArray(v) && typeof k === "number") return v.map((x: Json, i) => (i === k ? go(x, more) : x));
    if (typeof v === "object" && v !== null && !Array.isArray(v))
      return k === undefined
        ? Object.fromEntries(Object.entries(v).filter(([name]) => name !== key))
        : Object.fromEntries(Object.entries(v).map(([name, x]) => [name, name === k ? go(x, more) : x]));
    throw new Error(`no ${String(k)}`);
  };
  return go(json, from);
}

describe("decodeData", () => {
  test("decodes the real sample", () => {
    const d = fresh();
    expect(d.title).toBe("Mannheim Hbf → Frankfurt (Main) Hauptbahnhof");
    expect(d.metrics.map((m) => m.id)).toEqual(["remaining-time", "fastest-real", "hyperbolic", "axis", "ratio"]);
    const { info, ...piece } = d.pieces[2] ?? { info: "" };
    expect(info).toStartWith("U1 (10001) → Frankfurt (Main) Ginnheim Mitte");
    expect(piece).toEqual({
      kind: "ride",
      warn: false,
      trip: 3,
      pts: [
        [29881990, 2],
        [29881991, 8],
        [29881991, 8],
        [29881992, 9],
      ],
    });
    expect(d.chains[0]?.fallbacks).toEqual([
      [0, "10:29 (+14m)"],
      [1, "10:35 (+20m)"],
    ]);
  });

  test("rejects malformed data, naming where", () => {
    const cases: [Json, string][] = [
      [patched(["pieces", 3, "kind"], "bus"), "data.pieces[3].kind: expected one of ride, wait, walk"],
      [patched(["pieces", 2, "pts", 1], [29881991, 8, 0]), "data.pieces[2].pts[1]: expected a pair"],
      [patched(["pieces", 2, "pts", 1, 1], "8"), "data.pieces[2].pts[1][1]: expected a number"],
      [patched(["pieces", 0, "trip"], "0"), "data.pieces[0].trip: expected a number"],
      [patched(["metrics", 1, "heights", 4], "2"), "data.metrics[1].heights[4]: expected a number"],
      [patched(["chains", 0, "fallbacks", 1], [1]), "data.chains[0].fallbacks[1]: expected a pair"],
      [patched(["fallbacks", 1, "pieces", 0], null), "data.fallbacks[1].pieces[0]: expected a number"],
      [patched(["groups", 0, "count"], "11"), "data.groups[0].count: expected a number"],
      [patched(["back"], 1), "data.back: expected a string"],
      [without("tz", []), "data.tz: expected a value"],
      [without("route", ["chains", 0]), "data.chains[0].route: expected a value"],
      [without("warn", ["pieces", 5]), "data.pieces[5].warn: expected a value"],
    ];
    for (const [value, message] of cases) expect(() => decodeData(value, "data")).toThrow(message);
  });

  test("nullable fields take null", () => {
    expect(decodeData(patched(["back"], null), "data").back).toBeNull();
    expect(decodeData(patched(["metrics", 0, "heights", 3], null), "data").metrics[0]?.heights[3]).toBeNull();
  });
});

describe("load", () => {
  test("minutes become milliseconds; fallbacks are the plot's pieces with the chain's arrival", () => {
    const plot = load(fresh());
    expect(plot.pieces[0]?.raw).toEqual([
      [29881949 * MIN, 0],
      [29881986 * MIN, 2],
    ]);
    const [first, second] = plot.chains[0]?.fallbacks ?? [];
    expect(first?.arrive).toBe("10:29 (+14m)");
    expect(second?.line).toBe(
      "10:28 U3 (30008) → 10:30 Frankfurt (Main) Willy-Brandt-Platz ‖3m⚠‖ 10:33 U4 (40001) → 10:35 Frankfurt (Main) Hauptbahnhof",
    );
    // Shared, so heights applied to the plot's pieces show in the fallbacks.
    expect(first?.pieces.map((p) => plot.pieces.indexOf(p))).toEqual([5, 6]);
    expect(second?.pieces.map((p) => plot.pieces.indexOf(p))).toEqual([7, 8, 9, 10]);
  });

  test("an out-of-range piece index fails", () => {
    expect(() => load(decodeData(patched(["fallbacks", 0, "pieces", 1], 99), "data"))).toThrow("index 99 out of range");
  });
});

describe("applyHeights", () => {
  test("points get the metric's heights; rows highest first", () => {
    const plot = load(fresh());
    const rows = applyHeights(plot, 0);
    // remaining-time: Mannheim 36, Südbahnhof 4, … Frankfurt Hbf 0.
    expect(rows[0]).toEqual({ i: 0, name: "Mannheim Hbf", y: 36 });
    expect(rows.at(-1)).toEqual({ i: 1, name: "Frankfurt (Main) Hauptbahnhof", y: 0 });
    expect(rows.map((r) => r.y)).toEqual([...rows.map((r) => r.y)].sort((a, b) => b - a));
    expect(plot.pieces[0]?.pts).toEqual([
      [29881949 * MIN, 36, 0],
      [29881986 * MIN, 4, 2],
    ]);
    // Switching metric replaces the heights.
    applyHeights(plot, 4);
    expect(plot.pieces[0]?.pts.map((p) => p[1])).toEqual([100, 2.62]);
  });

  test("equal heights are ordered by name", () => {
    const plot = load(fresh());
    // remaining-time 2: Ostendstraße (4), Hauptwache (6), Willy-Brandt-Platz (9).
    const twos = applyHeights(plot, 0).filter((r) => r.y === 2);
    expect(twos.map((r) => r.name)).toEqual([
      "Frankfurt (Main) Hauptwache",
      "Frankfurt (Main) Ostendstraße",
      "Frankfurt (Main) Willy-Brandt-Platz",
    ]);
  });

  test("stations without a height are left out of rows and lines", () => {
    // No height for Südbahnhof (2), where the connection changes.
    const plot = load(decodeData(patched(["metrics", 0, "heights", 2], null), "data"));
    const rows = applyHeights(plot, 0);
    expect(rows.map((r) => r.i)).not.toContain(2);
    expect(rows).toHaveLength(9);
    expect(plot.pieces[0]?.pts).toEqual([[29881949 * MIN, 36, 0]]);
    expect(plot.pieces[1]?.pts).toEqual([]);
  });
});

test("extent: the connections' times padded by 10 minutes, heights from 0, fallbacks ignored", () => {
  const plot = load(fresh());
  applyHeights(plot, 0);
  // The connection runs 09:29–10:15; its fallbacks until 10:35.
  expect(extent(plot)).toEqual({ t0: (29881949 - 10) * MIN, t1: (29881995 + 10) * MIN, y0: 0, y1: 36 });
});

test("reach on the sample: on the ICE, the rest of the connection lies ahead", () => {
  const plot = load(fresh());
  applyHeights(plot, 0);
  const net = network(plot.pieces, plot.chains, () => true);
  const ice = net.rides[0];
  if (!ice) throw new Error("no ride");
  const [a] = ice.pts;
  const reached = net.reach({ at: [], back: [[a[2], a[0]]], ride: { trip: ice.trip, from: a[0], until: a[0] } });
  // Every piece after the ICE, whole; the fallbacks are no connection of their own.
  expect(reached.map(([pi, from, to]) => [pi, (from / MIN) % 1000, (to / MIN) % 1000]).sort()).toEqual([
    [1, 986, 990],
    [2, 990, 992],
    [3, 992, 993],
    [4, 993, 995],
  ]);
});
