import { describe, expect, test } from "bun:test";
import { parseJson } from "./decode";
import {
  type Field,
  type Kind,
  advancedOpen,
  decodeSpec,
  given,
  initial,
  label,
  wallTime,
  omitted,
  picked,
  sections,
  stationSearch,
  decodeStops,
  unpicked,
  zoneOf,
} from "./fields";

const f = (name: string, kind: Kind, def = "", advanced = false): Field => ({
  name,
  help: "",
  kind,
  default: def,
  choices: [],
  advanced,
});

describe("decodeSpec", () => {
  const spec = (fields: string): string => `{"fields": ${fields}, "values": {"from": "Mannheim"}, "error": null}`;

  test("decodes fields, values and error", () => {
    const s = decodeSpec(
      parseJson(
        spec(
          '[{"name": "sort", "help": "Order", "kind": "choice", "default": "arrival", "choices": ["arrival", "changes"], "advanced": true}]',
        ),
      ),
      "spec",
    );
    expect(s).toEqual({
      fields: [
        {
          name: "sort",
          help: "Order",
          kind: "choice",
          default: "arrival",
          choices: ["arrival", "changes"],
          advanced: true,
        },
      ],
      values: { from: "Mannheim" },
      error: null,
    });
  });

  test("names the failing field", () => {
    const field = (kind: string): string =>
      spec(`[{"name": "a", "help": "", "kind": "text", "default": "", "choices": [], "advanced": false},
             {"name": "b", "help": "", "kind": ${kind}, "default": "", "choices": [], "advanced": false}]`);
    expect(() => decodeSpec(parseJson(field('"date"')), "spec")).toThrow(
      "spec.fields[1].kind: expected one of text, number, flag, choice, time, zone, station, stations",
    );
    expect(() => decodeSpec(parseJson('{"fields": [], "values": {"max_changes": 3}, "error": null}'), "spec")).toThrow(
      "spec.values.max_changes: expected a string",
    );
    expect(() => decodeSpec(parseJson('{"fields": [], "values": {}}'), "spec")).toThrow("spec.error: expected a value");
  });
});

test("label", () => {
  expect(label("max_changes")).toBe("Max changes");
  expect(label("from")).toBe("From");
  expect(label("all_stops_now")).toBe("All stops now");
});

test("given: only the URL's own values", () => {
  const values = { at: "2026-10-25T08:00" };
  expect(given(values, "at")).toBe("2026-10-25T08:00");
  expect(given(values, "to")).toBeUndefined();
  expect(given(values, "constructor")).toBeUndefined();
});

describe("initial", () => {
  // 2026-01-02 03:04 UTC: 04:04 in Berlin, 22:04 the day before in New York.
  const now = new Date(Date.UTC(2026, 0, 2, 3, 4));
  const zone = f("time_zone", "zone");
  const cases: [string, Field, Record<string, string>, string | boolean][] = [
    ["a flag is off unless given", f("all_stops", "flag", "true"), {}, false],
    ["a given flag is on", f("all_stops", "flag"), { all_stops: "" }, true],
    ["the zone is the user's", zone, {}, "Europe/Berlin"],
    ["a given zone over the user's", zone, { time_zone: "America/New_York" }, "America/New_York"],
    ["a time not given is now in the user's zone", f("at", "time", "2026-10-25T08:00"), {}, "2026-01-02T04:04"],
    ["or in the given zone", f("at", "time"), { time_zone: "America/New_York" }, "2026-01-01T22:04"],
    ["a given time", f("at", "time"), { at: "2026-10-25T08:00", time_zone: "Asia/Kolkata" }, "2026-10-25T08:00"],
    ["the default", f("max_changes", "number", "3"), {}, "3"],
    ["a given value over the default", f("max_changes", "number", "3"), { max_changes: "5" }, "5"],
    ["an empty given value over the default", f("via", "stations", "Fulda"), { via: "" }, ""],
  ];
  for (const [name, field, values, expected] of cases)
    test(name, () => {
      expect(initial(field, values, now, "Europe/Berlin")).toBe(expected);
    });
});

test("zoneOf: the URL's zone, else the user's", () => {
  expect(zoneOf({}, "Europe/Berlin")).toBe("Europe/Berlin");
  expect(zoneOf({ time_zone: "Asia/Kolkata" }, "Europe/Berlin")).toBe("Asia/Kolkata");
});

test("wallTime: the time in the zone, padded, midnight as 00", () => {
  const t = new Date(Date.UTC(2026, 8, 5, 7, 3));
  expect(wallTime(t, "UTC")).toBe("2026-09-05T07:03");
  expect(wallTime(t, "Europe/Berlin")).toBe("2026-09-05T09:03"); // CEST
  expect(wallTime(t, "Asia/Kolkata")).toBe("2026-09-05T12:33");
  expect(wallTime(t, "America/New_York")).toBe("2026-09-05T03:03");
  expect(wallTime(new Date(Date.UTC(2026, 11, 31, 23, 0)), "Europe/Berlin")).toBe("2027-01-01T00:00");
  expect(wallTime(new Date(Date.UTC(2026, 11, 31, 23, 59)), "UTC")).toBe("2026-12-31T23:59");
});

test("sections: the trip in its own order, then limits and advanced in spec order", () => {
  const fields = [
    f("max_changes", "number"),
    f("via", "stations"),
    f("to", "station"),
    f("parallel", "number", "2", true),
    f("from", "station"),
    f("window", "number"),
    f("at", "time"),
    f("metric", "choice"),
    f("verbose", "flag", "", true),
    f("time_zone", "zone"),
  ];
  const s = sections(fields);
  const names = (xs: Field[]): string[] => xs.map((x) => x.name);
  expect(names(s.trip)).toEqual(["from", "to", "at", "time_zone", "window", "via"]);
  expect(names(s.limits)).toEqual(["max_changes", "metric"]);
  expect(names(s.advanced)).toEqual(["parallel", "verbose"]);
});

test("sections: a trip field given twice is shown once, the first", () => {
  const s = sections([f("from", "station", "A"), f("from", "station", "B")]);
  expect(s.trip.map((x) => x.default)).toEqual(["A"]);
  expect([...s.limits, ...s.advanced]).toEqual([]);
});

describe("advancedOpen", () => {
  const fields = [f("parallel", "number", "2", true), f("max_changes", "number", "3")];
  const cases: [string, Record<string, string>, boolean][] = [
    ["nothing given", {}, false],
    ["advanced at its default", { parallel: "2" }, false],
    ["advanced changed", { parallel: "4" }, true],
    ["only a plain field changed", { max_changes: "5" }, false],
  ];
  for (const [name, values, expected] of cases)
    test(name, () => {
      expect(advancedOpen(fields, values)).toBe(expected);
    });
});

describe("omitted", () => {
  const cases: [Field, string, boolean][] = [
    [f("max_changes", "number", "3"), "3", true],
    [f("max_changes", "number", "3"), "3.0", false],
    [f("via", "stations", ""), "", true],
    [f("via", "stations", ""), "Fulda", false],
    [f("metric", "choice", "time"), "time", true],
    // Flags and required stations are always sent as they are.
    [f("all_stops", "flag", "on"), "on", false],
    [f("from", "station", "Mannheim"), "Mannheim", false],
  ];
  for (const [field, value, expected] of cases)
    test(`${field.kind} ${field.name}=${JSON.stringify(value)}`, () => {
      expect(omitted(field, value)).toBe(expected);
    });
});

describe("stationSearch", () => {
  const cases: [string, string | null][] = [
    ["  Mannheim Hbf ", "Mannheim Hbf"],
    ["Ma", null],
    [" Ma ", null],
    ["Man", "Man"],
  ];
  for (const [text, expected] of cases)
    test(JSON.stringify(text), () => {
      expect(stationSearch(text)).toEqual(expected);
    });
});

describe("decodeStops", () => {
  test("names with their ids", () => {
    expect(decodeStops(parseJson('[{"name": "Fulda", "id": "de:1"}]'), "stops")).toEqual([
      { name: "Fulda", id: "de:1" },
    ]);
  });
  test("an id is required", () => {
    expect(() => decodeStops(parseJson('[{"name": "Fulda"}]'), "stops")).toThrow("stops[0].id");
  });
});

describe("picked and unpicked stations", () => {
  const cases: [Record<string, string>, { name: string; id: string }[], string[]][] = [
    [{ from: "Fulda", from_id: "f" }, [{ name: "Fulda", id: "f" }], []],
    // An older link: the name only, to look up.
    [{ from: " Fulda " }, [], ["Fulda"]],
    [{}, [], []],
    // Lists pair names and ids in order, blanks left out.
    [
      { from: "Fulda; Kassel-Wilhelmshöhe ;", from_id: " f;k" },
      [
        { name: "Fulda", id: "f" },
        { name: "Kassel-Wilhelmshöhe", id: "k" },
      ],
      [],
    ],
    // Ids that don't pair up pick nothing.
    [{ from: "Fulda;Kassel", from_id: "f" }, [], ["Fulda", "Kassel"]],
    [{ from_id: "f" }, [], []],
  ];
  for (const [values, stops, names] of cases)
    test(JSON.stringify(values), () => {
      expect(picked(values, "from")).toEqual(stops);
      expect(unpicked(values, "from")).toEqual(names);
    });
});
