import { describe, expect, test } from "bun:test";
import { type Connection, type Step, type Stretch, callsOf } from "./best";
import { Fallbacks, arrive, byText, rejoins, standing, transfers } from "./fallback";

// The server's test timetables (src/fallback_tests.rs), times "HH:MM" in ms.
const ms = (hhmm: string): number => {
  const [h, m] = hhmm.split(":").map(Number);
  return ((h ?? 0) * 60 + (m ?? 0)) * 60e3;
};
const clock = (t: number): string =>
  `${String(Math.floor(t / 3600e3)).padStart(2, "0")}:${String((t / 60e3) % 60).padStart(2, "0")}`;

/** Stations by name; trains as (station, arrival, departure), "" where there is none. */
class World {
  names: string[] = [];
  trips: string[] = [];
  pieces: Stretch[] = [];
  station(name: string): number {
    const i = this.names.indexOf(name);
    return i >= 0 ? i : this.names.push(name) - 1;
  }
  trip(label: string, stops: [string, string, string][]): void {
    const trip = this.trips.push(label) - 1;
    const raw: [number, number][] = [];
    for (const [name, a, d] of stops) {
      const s = this.station(name);
      if (a) raw.push([ms(a), s]);
      if (d) raw.push([ms(d), s]);
    }
    this.pieces.push({ kind: "ride", trip, raw });
  }
  walk(from: string, to: string, minutes: number): void {
    this.pieces.push({
      kind: "walk",
      trip: null,
      raw: [
        [0, this.station(from)],
        [minutes * 60e3, this.station(to)],
      ],
    });
  }
  /** A plan riding (train, from, to) in turn, walking between where asked ("walk"). */
  plan(legs: [string, string, string][]): Connection {
    const steps: Step[] = [];
    for (const [label, from, to] of legs) {
      if (label === "walk") {
        const last = steps[steps.length - 1];
        const t = last?.kind === "ride" ? (last.calls[last.calls.length - 1]?.arr ?? 0) : 0;
        const p = this.pieces.find(
          (q) => q.kind === "walk" && q.raw[0]?.[1] === this.station(from) && q.raw[1]?.[1] === this.station(to),
        );
        const d = (p?.raw[1]?.[0] ?? 0) - (p?.raw[0]?.[0] ?? 0);
        steps.push({ kind: "walk", from: this.station(from), to: this.station(to), start: t, end: t + d });
        continue;
      }
      const trip = this.trips.indexOf(label);
      const calls = callsOf(this.pieces.find((p) => p.trip === trip)?.raw ?? []);
      const a = calls.findIndex((c) => c.station === this.station(from)),
        b = calls.findIndex((c) => c.station === this.station(to));
      steps.push({ kind: "ride", trip, calls: calls.slice(a, b + 1) });
    }
    const rides = steps.flatMap((s) => (s.kind === "ride" ? [s] : []));
    const first = rides[0]?.calls[0]?.dep ?? 0;
    const last = rides[rides.length - 1];
    return {
      steps,
      departure: first,
      arrival: last?.calls[last.calls.length - 1]?.arr ?? 0,
      changes: rides.length - 1,
      slack: null,
    };
  }
  fallbacks(): Fallbacks {
    return new Fallbacks({
      pieces: this.pieces,
      network: this.pieces.map((_, i) => i),
      stations: this.names.length,
      limits: {
        origin: 0,
        destination: this.station(this.destination),
        from: 0,
        until: 0,
        maxDuration: 0,
        maxChanges: 0,
        missedBy: 5 * 60e3,
        minTransfer: 5 * 60e3,
        keepDetours: false,
      },
      names: this.names,
      trips: this.trips,
      clock,
    });
  }
  constructor(private readonly destination: string) {}
}

/** Hamburg Hbf around RJ 384 (12:00), which plans change to. */
function hamburg(): World {
  const w = new World("København");
  w.trip("ICE 2", [
    ["Frankfurt", "", "08:00"],
    ["Hamburg", "11:51", ""],
  ]);
  w.trip("IC 5", [
    ["Kiel", "", "10:30"],
    ["Hamburg", "11:40", ""],
  ]);
  w.trip("RJ 384", [
    ["Hamburg", "", "12:00"],
    ["Altona", "12:20", "12:25"],
    ["Schleswig", "13:30", "13:32"],
    ["København", "17:00", ""],
  ]);
  w.trip("S 1", [
    ["Hamburg", "", "12:05"],
    ["Altona", "12:15", ""],
  ]);
  // RJ 384 gone by a minute.
  w.trip("S 3", [
    ["Hamburg", "", "12:10"],
    ["Altona", "12:26", ""],
  ]);
  w.trip("RE 7", [
    ["Hamburg", "", "12:01"],
    ["Schleswig", "13:00", ""],
  ]);
  w.trip("RE 9", [
    ["Schleswig", "", "13:40"],
    ["København", "17:30", ""],
  ]);
  // Leaves with the missed train.
  w.trip("IC 1", [
    ["Hamburg", "", "12:00"],
    ["København", "16:00", ""],
  ]);
  w.trip("RJ 386", [
    ["Hamburg", "", "14:01"],
    ["København", "19:00", ""],
  ]);
  w.trip("RJ 388", [
    ["Hamburg", "", "14:02"],
    ["København", "18:10", ""],
  ]);
  // A plan changing at Altona at 12:00 too.
  w.trip("RB 10", [
    ["Kiel", "", "10:00"],
    ["Altona", "11:45", ""],
  ]);
  w.trip("RE 11", [
    ["Altona", "", "12:00"],
    ["København", "16:50", ""],
  ]);
  return w;
}

/** A → B by RE 1 (08:50), missing RE 2 from B at 09:00; trains from B as given. */
function missedAtB(...trains: [string, [string, string, string][]][]): { w: World; plan: Connection } {
  const w = new World("D");
  w.trip("RE 1", [
    ["A", "", "08:00"],
    ["B", "08:50", ""],
  ]);
  w.trip("RE 2", [
    ["B", "", "09:00"],
    ["D", "09:30", ""],
  ]);
  for (const [label, stops] of trains) w.trip(label, stops);
  const plan = w.plan([
    ["RE 1", "A", "B"],
    ["RE 2", "B", "D"],
  ]);
  return { w, plan };
}

const lines = (w: World, plan: Connection, change = 0): [string, string][] =>
  w
    .fallbacks()
    .forChange(plan, change)
    .map((a) => [a.line, clock(a.lands)]);

describe("fallbacks, as the server finds them", () => {
  test("ranked by landing, back on plan counting its arrival; changes and buffer next", () => {
    const w = hamburg();
    const plan = w.plan([
      ["ICE 2", "Frankfurt", "Hamburg"],
      ["RJ 384", "Hamburg", "København"],
    ]);
    // 32 min at Schleswig beats 10 at Altona; RJ 386 at 14:01 is the window's end.
    expect(lines(w, plan)).toEqual([
      ["12:01 RE 7 → 13:00 Schleswig ‖32m‖ 13:32 RJ 384 (back on plan)", "17:00"],
      ["12:05 S 1 → 12:15 Altona ‖10m‖ 12:25 RJ 384 (back on plan)", "17:00"],
      ["12:01 RE 7 → 13:00 Schleswig ‖40m‖ 13:40 RE 9 → 17:30 København", "17:30"],
    ]);
  });

  test("catching the plan's train counts only before the plan leaves it", () => {
    const w = hamburg();
    const plan = w.plan([
      ["IC 5", "Kiel", "Hamburg"],
      ["RJ 384", "Hamburg", "Schleswig"],
      ["RE 9", "Schleswig", "København"],
    ]);
    expect(lines(w, plan)).toEqual([
      ["12:01 RE 7 → 13:00 Schleswig ‖32m‖ 13:32 RJ 384 → 17:00 København", "17:00"],
      ["12:01 RE 7 → 13:00 Schleswig ‖40m‖ 13:40 RE 9 (back on plan)", "17:30"],
      ["12:05 S 1 → 12:15 Altona ‖10m‖ 12:25 RJ 384 (back on plan)", "17:30"],
    ]);
  });

  test("a plan train caught after the plan left it lands on its own", () => {
    const w = new World("D");
    w.trip("RE 1", [
      ["A", "", "08:00"],
      ["B", "08:50", ""],
    ]);
    w.trip("ICE 2", [
      ["B", "", "09:00"],
      ["C", "09:30", "09:31"],
      ["E", "10:00", "10:01"],
      ["D", "10:40", ""],
    ]);
    w.trip("ICE 3", [
      ["C", "", "09:40"],
      ["D", "10:30", ""],
    ]);
    w.trip("RB 4", [
      ["B", "", "09:10"],
      ["E", "09:45", ""],
    ]);
    w.trip("RB 5", [
      ["B", "", "09:05"],
      ["C", "09:20", ""],
    ]);
    const plan = w.plan([
      ["RE 1", "A", "B"],
      ["ICE 2", "B", "C"],
      ["ICE 3", "C", "D"],
    ]);
    expect(lines(w, plan)).toEqual([
      ["09:05 RB 5 → 09:20 C ‖20m‖ 09:40 ICE 3 (back on plan)", "10:30"],
      ["09:10 RB 4 → 09:45 E ‖16m‖ 10:01 ICE 2 → 10:40 D", "10:40"],
      ["09:05 RB 5 → 09:20 C ‖11m‖ 09:31 ICE 2 → 10:40 D", "10:40"],
    ]);
  });

  test("nothing within two hours after the missed train: none", () => {
    const w = new World("D");
    w.trip("RE 1", [
      ["A", "", "08:00"],
      ["B", "08:50", ""],
    ]);
    w.trip("RE 2", [
      ["B", "", "09:00"],
      ["D", "09:30", ""],
    ]);
    w.trip("RE 3", [
      ["B", "", "11:02"],
      ["D", "11:30", ""],
    ]);
    const plan = w.plan([
      ["RE 1", "A", "B"],
      ["RE 2", "B", "D"],
    ]);
    expect(lines(w, plan)).toEqual([]);
    expect(() => w.fallbacks().forChange(plan, 1)).toThrow();
  });

  test("after a change with a walk, fallbacks leave from where the train arrived, walking too", () => {
    const w = new World("Ziel");
    w.trip("STR 4", [
      ["Dorf", "", "08:00"],
      ["Zentrum", "08:30", ""],
    ]);
    w.walk("Zentrum", "Hbf", 8);
    w.trip("ICE 1", [
      ["Hbf", "", "09:00"],
      ["Ziel", "10:00", ""],
    ]);
    w.trip("ICE 3", [
      ["Hbf", "", "09:30"],
      ["Ziel", "10:30", ""],
    ]);
    const plan = w.plan([
      ["STR 4", "Dorf", "Zentrum"],
      ["walk", "Zentrum", "Hbf"],
      ["ICE 1", "Hbf", "Ziel"],
    ]);
    expect(transfers(plan)).toEqual([{ station: w.station("Zentrum"), departure: ms("09:00"), ready: ms("08:38") }]);
    const [only, ...rest] = w.fallbacks().forChange(plan, 0);
    expect(rest).toEqual([]);
    expect([only?.line, only && clock(only.lands)]).toEqual(["walk 8m to Hbf 09:30 ICE 3 → 10:30 Ziel", "10:30"]);
    // Drawn from the missed departure: the walk then, a wait after the missed train, the ride.
    expect(only?.pieces.map((p) => [p.kind, p.info, p.warn, p.raw.map(([t]) => clock(t))])).toEqual([
      ["walk", "walk 8m to Hbf", false, ["09:00", "09:08"]],
      ["wait", "22m at Hbf after the missed train", false, ["09:08", "09:30"]],
      ["ride", "ICE 3 · 09:30 Hbf → 10:30 Ziel", false, ["09:30", "10:30"]],
    ]);
  });

  test("a change missed already (missed-by): fallbacks leave once one is there", () => {
    const w = new World("København");
    w.trip("ICE 2", [
      ["Frankfurt", "", "08:00"],
      ["Hamburg", "12:04", ""],
    ]);
    w.trip("RJ 384", [
      ["Hamburg", "", "12:00"],
      ["Schleswig", "13:30", "13:32"],
      ["København", "17:00", ""],
    ]);
    w.trip("RE 7", [
      ["Hamburg", "", "12:01"],
      ["Schleswig", "13:00", ""],
    ]);
    w.trip("S 1", [
      ["Hamburg", "", "12:05"],
      ["Schleswig", "13:20", ""],
    ]);
    const plan = w.plan([
      ["ICE 2", "Frankfurt", "Hamburg"],
      ["RJ 384", "Hamburg", "København"],
    ]);
    expect(lines(w, plan)).toEqual([["12:05 S 1 → 13:20 Schleswig ‖12m‖ 13:32 RJ 384 (back on plan)", "17:00"]]);
  });

  test("the same candidates serve every plan changing there", () => {
    const w = hamburg();
    const shared = w.fallbacks();
    const a = w.plan([
      ["ICE 2", "Frankfurt", "Hamburg"],
      ["RJ 384", "Hamburg", "København"],
    ]);
    const b = w.plan([
      ["IC 5", "Kiel", "Hamburg"],
      ["RJ 384", "Hamburg", "Schleswig"],
      ["RE 9", "Schleswig", "København"],
    ]);
    for (const p of [a, b, a]) expect(shared.forChange(p, 0).map((f) => f.line)).toEqual(lines(w, p).map(([l]) => l));
  });

  test("candidates per station and time: each plan its own", () => {
    const w = hamburg();
    const shared = w.fallbacks();
    const atHamburg = w.plan([
      ["ICE 2", "Frankfurt", "Hamburg"],
      ["RJ 384", "Hamburg", "København"],
    ]);
    // Missing a 12:00 departure too, at Altona.
    const atAltona = w.plan([
      ["RB 10", "Kiel", "Altona"],
      ["RE 11", "Altona", "København"],
    ]);
    const first = shared.forChange(atHamburg, 0).map((f) => f.line);
    expect(shared.forChange(atAltona, 0).map((f) => f.line)).toEqual(lines(w, atAltona).map(([l]) => l));
    expect(shared.forChange(atAltona, 0).map((f) => f.line)).not.toEqual(first);
  });

  test("no detours: a train is taken where it is first caught", () => {
    // T calls at B at 09:30 too: S to C for it is a detour.
    const { w, plan } = missedAtB(
      [
        "T",
        [
          ["B", "", "09:30"],
          ["C", "09:40", "09:41"],
          ["D", "10:30", ""],
        ],
      ],
      [
        "S",
        [
          ["B", "", "09:05"],
          ["C", "09:15", ""],
        ],
      ],
    );
    expect(lines(w, plan)).toEqual([["09:30 T → 10:30 D", "10:30"]]);
  });

  test("at most an hour slower than the fastest", () => {
    const { w, plan } = missedAtB(
      [
        "X",
        [
          ["B", "", "09:10"],
          ["D", "09:40", ""],
        ],
      ],
      [
        "Y",
        [
          ["B", "", "09:20"],
          ["D", "10:50", ""],
        ],
      ],
      [
        "Z",
        [
          ["B", "", "09:30"],
          ["D", "11:01", ""],
        ],
      ],
    );
    expect(lines(w, plan)).toEqual([
      ["09:10 X → 09:40 D", "09:40"],
      ["09:20 Y → 10:50 D", "10:50"],
    ]);
  });

  test("same landing: fewer changes, then the bigger buffer, then the line", () => {
    const { w, plan } = missedAtB(
      [
        "Direct",
        [
          ["B", "", "09:30"],
          ["D", "10:30", ""],
        ],
      ],
      [
        "S 1",
        [
          ["B", "", "09:05"],
          ["C", "09:15", ""],
        ],
      ],
      [
        "S 2",
        [
          ["B", "", "09:06"],
          ["E", "09:16", ""],
        ],
      ],
      [
        "RB 3",
        [
          ["C", "", "09:45"],
          ["D", "10:30", ""],
        ],
      ],
      [
        "RB 4",
        [
          ["E", "", "09:26"],
          ["D", "10:30", ""],
        ],
      ],
    );
    // Both changes land at 10:30: 30 minutes at C beat 10 at E; the direct one first.
    expect(lines(w, plan)).toEqual([
      ["09:30 Direct → 10:30 D", "10:30"],
      ["09:05 S 1 → 09:15 C ‖30m‖ 09:45 RB 3 → 10:30 D", "10:30"],
      ["09:06 S 2 → 09:16 E ‖10m‖ 09:26 RB 4 → 10:30 D", "10:30"],
    ]);
    // All else equal, by the line, whatever order they are found in.
    const same: [string, string, string][] = [
      ["B", "", "09:30"],
      ["D", "10:30", ""],
    ];
    const tie = missedAtB(["RE 8", same], ["RE 9", same], ["RE 7", same]);
    expect(lines(tie.w, tie.plan).map(([l]) => l)).toEqual([
      "09:30 RE 7 → 10:30 D",
      "09:30 RE 8 → 10:30 D",
      "09:30 RE 9 → 10:30 D",
    ]);
  });

  test("back on plan on the first train; not where the plan leaves the train as it leaves", () => {
    // Plan: RE 1 A→B, RE 2 B→C (missed), T C→X (left at X at 10:00, where it
    // leaves at 10:00), R 4 X→D. T also calls at B at 09:20.
    const w = new World("D");
    w.trip("RE 1", [
      ["A", "", "08:00"],
      ["B", "08:50", ""],
    ]);
    w.trip("RE 2", [
      ["B", "", "09:00"],
      ["C", "09:30", ""],
    ]);
    w.trip("T", [
      ["B", "", "09:20"],
      ["C", "09:39", "09:40"],
      ["X", "10:00", "10:00"],
      ["D", "10:40", ""],
    ]);
    w.trip("R 4", [
      ["X", "", "10:10"],
      ["D", "10:50", ""],
    ]);
    w.trip("S", [
      ["B", "", "09:05"],
      ["X", "09:55", ""],
    ]);
    const plan = w.plan([
      ["RE 1", "A", "B"],
      ["RE 2", "B", "C"],
      ["T", "C", "X"],
      ["R 4", "X", "D"],
    ]);
    expect(lines(w, plan)).toEqual([
      // T from B: back on plan right away, no change; T from X after S would be a detour.
      ["09:20 T (back on plan)", "10:50"],
      ["09:05 S → 09:55 X ‖15m‖ 10:10 R 4 (back on plan)", "10:50"],
    ]);
    // Without T from B: S to X, then T leaving X as the plan leaves it is on its own.
    const v = new World("D");
    const trains: [string, [string, string, string][]][] = [
      [
        "RE 1",
        [
          ["A", "", "08:00"],
          ["B", "08:50", ""],
        ],
      ],
      [
        "RE 2",
        [
          ["B", "", "09:00"],
          ["C", "09:30", ""],
        ],
      ],
      [
        "T",
        [
          ["C", "", "09:40"],
          ["X", "10:00", "10:00"],
          ["D", "10:40", ""],
        ],
      ],
      [
        "R 4",
        [
          ["X", "", "10:10"],
          ["D", "10:50", ""],
        ],
      ],
      [
        "S",
        [
          ["B", "", "09:05"],
          ["X", "09:55", ""],
        ],
      ],
    ];
    for (const [label, stops] of trains) v.trip(label, stops);
    const plan2 = v.plan([
      ["RE 1", "A", "B"],
      ["RE 2", "B", "C"],
      ["T", "C", "X"],
      ["R 4", "X", "D"],
    ]);
    expect(lines(v, plan2)).toEqual([
      ["09:05 S → 09:55 X ‖5m‖ 10:00 T → 10:40 D", "10:40"],
      ["09:05 S → 09:55 X ‖15m‖ 10:10 R 4 (back on plan)", "10:50"],
    ]);
  });

  test("drawn: the wait after the missed train, changes as usual, up to the plan's train", () => {
    const w = hamburg();
    const plan = w.plan([
      ["ICE 2", "Frankfurt", "Hamburg"],
      ["RJ 384", "Hamburg", "København"],
    ]);
    const [back, , own] = w.fallbacks().forChange(plan, 0);
    expect(back?.pieces.map((p) => [p.kind, p.info, p.warn])).toEqual([
      ["wait", "1m at Hamburg after the missed train", false],
      ["ride", "RE 7 · 12:01 Hamburg → 13:00 Schleswig", false],
      ["wait", "32m at Schleswig", false],
    ]);
    expect(own?.pieces.map((p) => [p.kind, p.info, p.warn])).toEqual([
      ["wait", "1m at Hamburg after the missed train", false],
      ["ride", "RE 7 · 12:01 Hamburg → 13:00 Schleswig", false],
      ["wait", "40m at Schleswig", false],
      ["ride", "RE 9 · 13:40 Schleswig → 17:30 København", false],
    ]);
  });

  test("standing: back on plan ends the count; changes and buffer up to there", () => {
    // Plan: ICE 2 to Hamburg, RJ 384 on (left at København at 17:00).
    const w = hamburg();
    const plan = w.plan([
      ["ICE 2", "Frankfurt", "Hamburg"],
      ["RJ 384", "Hamburg", "København"],
    ]);
    const rejoin = rejoins(plan);
    expect([...rejoin.entries()].map(([t, at]) => [w.trips[t], clock(at)])).toEqual([
      ["ICE 2", "11:51"],
      ["RJ 384", "17:00"],
    ]);
    // S 1 to Altona (10 min), RJ 384 to Schleswig, RE 99 (3 min): back on plan
    // at the second train; the change after it doesn't count.
    w.trip("RE 99", [
      ["Schleswig", "", "13:33"],
      ["København", "16:59", ""],
    ]);
    const back = w.plan([
      ["S 1", "Hamburg", "Altona"],
      ["RJ 384", "Altona", "Schleswig"],
      ["RE 99", "Schleswig", "København"],
    ]);
    expect(standing(back, rejoin, ms("17:00"))).toEqual({
      back: 1,
      changes: 1,
      slack: ms("00:10"),
      lands: ms("17:00"),
    });
    // RE 7 to Schleswig (40 min), RE 9: on its own, both changes count.
    const own = w.plan([
      ["RE 7", "Hamburg", "Schleswig"],
      ["RE 9", "Schleswig", "København"],
    ]);
    expect(standing(own, rejoin, ms("17:00"))).toEqual({
      back: null,
      changes: 1,
      slack: ms("00:40"),
      lands: ms("17:30"),
    });
    // Direct: no change, no buffer to compare.
    const direct = w.plan([["RJ 386", "Hamburg", "København"]]);
    expect(standing(direct, rejoin, ms("17:00"))).toEqual({
      back: null,
      changes: 0,
      slack: Infinity,
      lands: ms("19:00"),
    });
  });

  test("text order: before, after, the same", () => {
    expect([byText("RE 7", "RE 8"), byText("RE 8", "RE 7"), byText("RE 8", "RE 8"), byText("Z", "a")]).toEqual([
      -1, 1, 0, -1,
    ]);
  });

  test("arrive: when it lands and how much later than planned", () => {
    expect([arrive(ms("17:30"), ms("17:00"), clock), arrive(ms("16:50"), ms("17:00"), clock)]).toEqual([
      "17:30 (+30m)",
      "16:50 (+0m)",
    ]);
  });
});
