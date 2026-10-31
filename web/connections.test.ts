import { describe, expect, test } from "bun:test";
import type { Connection } from "./best";
import { buffer, duration, header, piecesOf, route, signature, signatureOfPieces, trainName } from "./connections";

const M = 60e3;
const clock = (ms: number): string =>
  `${String(Math.floor(ms / M / 60)).padStart(2, "0")}:${String((ms / M) % 60).padStart(2, "0")}`;
const names = ["Origin", "Bus stop", "Mannheim, Hauptbahnhof", "Mannheim Bahnsteig 9", "Hamburg Hbf", "Hamburg Hbf"];
const trips = ["ICE 374 → Berlin", "ICE 593 → München Hbf"];

// Walk 5 min to the bus stop, ICE 374 at 09:00 via a stop to Mannheim (11:00),
// walk 2 min to platform 9, ICE 593 at 11:10 to Hamburg (14:00), walk on 3 min.
const trip: Connection = {
  steps: [
    { kind: "walk", from: 0, to: 1, start: 535 * M, end: 540 * M },
    {
      kind: "ride",
      trip: 0,
      calls: [
        { station: 1, arr: null, dep: 540 * M },
        { station: 4, arr: 600 * M, dep: 602 * M },
        { station: 2, arr: 660 * M, dep: null },
      ],
    },
    { kind: "walk", from: 2, to: 3, start: 660 * M, end: 662 * M },
    {
      kind: "ride",
      trip: 1,
      calls: [
        { station: 3, arr: null, dep: 670 * M },
        { station: 4, arr: 840 * M, dep: null },
      ],
    },
    { kind: "walk", from: 4, to: 5, start: 840 * M, end: 843 * M },
  ],
  departure: 535 * M,
  arrival: 843 * M,
  changes: 1,
  slack: 8 * M,
};

const firstRide = trip.steps[1] ?? trip.steps[0] ?? { kind: "walk", from: 0, to: 0, start: 0, end: 0 };

describe("words", () => {
  test("durations and buffers as the server writes them", () => {
    expect([duration(5 * M), duration(59 * M), duration(60 * M), duration(308 * M)]).toEqual([
      "5m",
      "59m",
      "1h00",
      "5h08",
    ]);
    expect([
      buffer(8 * M, 5 * M),
      buffer(4 * M, 5 * M),
      buffer(5 * M, 5 * M),
      buffer(0, 5 * M),
      buffer(-2 * M, 5 * M),
    ]).toEqual(["8m", "4m⚠", "5m", "0m⚠", "missed 2m✗"]);
    expect([trainName("ICE 593 → München Hbf"), trainName("RE 1")]).toEqual(["ICE 593", "RE 1"]);
  });

  test("header: leaving, arriving, duration, changes, worst buffer", () => {
    expect(header(trip, 5 * M, clock)).toBe("08:55 → 14:03  5h08  1×  min 8m");
    expect(header({ ...trip, changes: 0, slack: null }, 5 * M, clock)).toBe("08:55 → 14:03  5h08  0×  direct");
  });

  test("route: walks at the ends listed, a walk between trains part of the change", () => {
    expect(route(trip, names, trips, 5 * M, clock)).toBe(
      "walk 5m to Bus stop 09:00 ICE 374 → 11:00 Mannheim, Hauptbahnhof ‖8m walk 2m‖ 11:10 Mannheim Bahnsteig 9 ICE 593 → 14:00 Hamburg Hbf walk 3m to Hamburg Hbf",
    );
    // Boarding where the last train arrived: the station isn't named again.
    const direct: Connection = {
      ...trip,
      steps: [
        firstRide,
        {
          kind: "ride",
          trip: 1,
          calls: [
            { station: 2, arr: null, dep: 663 * M },
            { station: 4, arr: 840 * M, dep: null },
          ],
        },
      ],
    };
    expect(route(direct, names, trips, 5 * M, clock)).toBe(
      "09:00 ICE 374 → 11:00 Mannheim, Hauptbahnhof ‖3m⚠‖ 11:03 ICE 593 → 14:00 Hamburg Hbf",
    );
  });
});

describe("pieces", () => {
  test("rides from boarding to alighting, the wait after a walk, the walks", () => {
    const pieces = piecesOf(trip, names, trips, 5 * M, clock);
    expect(pieces.map((p) => [p.kind, p.info, p.warn, p.trip, p.raw.map(([t, s]) => [t / M, s])])).toEqual([
      [
        "walk",
        "walk 5m to Bus stop",
        false,
        null,
        [
          [535, 0],
          [540, 1],
        ],
      ],
      [
        "ride",
        "ICE 374 → Berlin · 09:00 Bus stop → 11:00 Mannheim, Hauptbahnhof",
        false,
        0,
        [
          [540, 1],
          [600, 4],
          [602, 4],
          [660, 2],
        ],
      ],
      [
        "walk",
        "walk 2m to Mannheim Bahnsteig 9",
        false,
        null,
        [
          [660, 2],
          [662, 3],
        ],
      ],
      [
        "wait",
        "8m at Mannheim Bahnsteig 9",
        false,
        null,
        [
          [662, 3],
          [670, 3],
        ],
      ],
      [
        "ride",
        "ICE 593 → München Hbf · 11:10 Mannheim Bahnsteig 9 → 14:00 Hamburg Hbf",
        false,
        1,
        [
          [670, 3],
          [840, 4],
        ],
      ],
      [
        "walk",
        "walk 3m to Hamburg Hbf",
        false,
        null,
        [
          [840, 4],
          [843, 5],
        ],
      ],
    ]);
  });

  test("a ride boarded and left where the train stands draws only the stretch ridden", () => {
    // ICE 374 boarded at its stop 4 (arrives 600, leaves 602) and left at 2,
    // where it stands until 665; a change of exactly the minimum is no warning.
    const middle: Connection = {
      ...trip,
      steps: [
        {
          kind: "ride",
          trip: 0,
          calls: [
            { station: 4, arr: 600 * M, dep: 602 * M },
            { station: 2, arr: 660 * M, dep: 665 * M },
          ],
        },
        {
          kind: "ride",
          trip: 1,
          calls: [
            { station: 2, arr: null, dep: 665 * M },
            { station: 5, arr: 700 * M, dep: null },
          ],
        },
      ],
    };
    const [ride, wait] = piecesOf(middle, names, trips, 5 * M, clock);
    expect(ride?.raw.map(([t, s]) => [t / M, s])).toEqual([
      [602, 4],
      [660, 2],
    ]);
    expect([wait?.info, wait?.warn]).toEqual(["5m at Mannheim, Hauptbahnhof", false]);
  });

  test("a missed change waits from the departure to the arrival, flagged", () => {
    const missed: Connection = {
      ...trip,
      steps: [
        firstRide,
        {
          kind: "ride",
          trip: 1,
          calls: [
            { station: 2, arr: null, dep: 657 * M },
            { station: 4, arr: 840 * M, dep: null },
          ],
        },
      ],
    };
    const wait = piecesOf(missed, names, trips, 5 * M, clock)[1];
    expect([wait?.info, wait?.warn, wait?.raw.map(([t]) => t / M)]).toEqual([
      "missed 3m✗ at Mannheim, Hauptbahnhof",
      true,
      [657, 660],
    ]);
  });

  test("a connection and its drawing have the same key, and no other has", () => {
    const drawn = piecesOf(trip, names, trips, 5 * M, clock);
    expect(signatureOfPieces(drawn)).toBe(signature(trip.steps));
    const later = { ...trip, steps: trip.steps.slice(1) };
    expect(signature(later.steps)).not.toBe(signature(trip.steps));
  });
});
