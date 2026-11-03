import { describe, expect, test } from "bun:test";
import { parseJson } from "./decode";
import {
  nextIndex,
  type Field,
  type Kind,
  changed,
  decodeSpec,
  given,
  initial,
  label,
  wallTime,
  omitted,
  picked,
  sections,
  stationSearch,
  decodeSuggestions,
  unpicked,
  sentTime,
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
    // A link dated in the second of a repeated hour carries the offset, which datetime-local would drop.
    [
      "a given time with an offset, as its wall time in the zone",
      f("at", "time"),
      { at: "2026-10-25T02:30+01:00", time_zone: "Europe/Berlin" },
      "2026-10-25T02:30",
    ],
    [
      "a given time with an offset that is no date, as given",
      f("at", "time"),
      { at: "2026-13-45T02:30+01:00" },
      "2026-13-45T02:30+01:00",
    ],
    [
      "a given time with an offset not at its end, as given",
      f("at", "time"),
      { at: "Sun Oct 25 2026 02:30:00 GMT+01:00 (CET)" },
      "Sun Oct 25 2026 02:30:00 GMT+01:00 (CET)",
    ],
    ["the default", f("max_changes", "number", "3"), {}, "3"],
    ["a given value over the default", f("max_changes", "number", "3"), { max_changes: "5" }, "5"],
    ["an empty given value over the default", f("via", "stations", "Fulda"), { via: "" }, ""],
  ];
  for (const [name, field, values, expected] of cases)
    test(name, () => {
      expect(initial(field, values, now, "Europe/Berlin")).toBe(expected);
    });
});

describe("sentTime", () => {
  // A link dated in the second 02:30 in Berlin on 2026-10-25; the field shows its wall time.
  const dated = "2026-10-25T02:30+01:00";
  const loaded = new Date(Date.UTC(2026, 0, 2, 3, 4));
  const cases: [string, string, string | undefined, string, string][] = [
    ["the given moment while the field shows it", "2026-10-25T02:30", dated, "Europe/Berlin", dated],
    ["a changed time as shown", "2026-10-25T03:30", dated, "Europe/Berlin", "2026-10-25T03:30"],
    ["a zone where the moment shows otherwise: as shown", "2026-10-25T02:30", dated, "UTC", "2026-10-25T02:30"],
    ["a zone that is none: as shown", "2026-10-25T02:30", dated, "Nowhere/Else", "2026-10-25T02:30"],
    ["no zone: as shown", "2026-10-25T02:30", dated, "", "2026-10-25T02:30"],
    ["a time given without an offset as shown", "2026-10-25T02:30", "2026-10-25T02:30", "UTC", "2026-10-25T02:30"],
    [
      "nothing given, a time other than now: as shown",
      "2026-10-25T02:30",
      undefined,
      "Europe/Berlin",
      "2026-10-25T02:30",
    ],
  ];
  for (const [name, shown, value, zone, expected] of cases)
    test(name, () => {
      expect(sentTime(shown, value, loaded, zone)).toBe(expected);
    });

  // Nothing given: the field starts as `now`, the moment the form was loaded.
  const utc = (iso: string): Date => new Date(`${iso}Z`);
  const nows: [string, string, Date, string, string][] = [
    ["now in the second 02:30, with its offset", "2026-10-25T02:30", utc("2026-10-25T01:30"), "Europe/Berlin", dated],
    ["seconds don't count", "2026-10-25T02:30", utc("2026-10-25T01:30:45"), "Europe/Berlin", dated],
    [
      "now in the first 02:30, as shown",
      "2026-10-25T02:30",
      utc("2026-10-25T00:30"),
      "Europe/Berlin",
      "2026-10-25T02:30",
    ],
    [
      "now in London's second 01:30",
      "2026-10-25T01:30",
      utc("2026-10-25T01:30"),
      "Europe/London",
      "2026-10-25T01:30+00:00",
    ],
    [
      "now in New York's second 01:30, behind UTC",
      "2026-11-01T01:30",
      utc("2026-11-01T06:30"),
      "America/New_York",
      "2026-11-01T01:30-05:00",
    ],
    [
      "now in Lord Howe's second 01:45, put back half an hour",
      "2026-04-05T01:45",
      utc("2026-04-04T15:15"),
      "Australia/Lord_Howe",
      "2026-04-05T01:45+10:30",
    ],
    [
      "now after the repeated hour, as shown",
      "2026-10-25T03:30",
      utc("2026-10-25T02:30"),
      "Europe/Berlin",
      "2026-10-25T03:30",
    ],
    [
      "now after the hour skipped in spring",
      "2026-03-29T03:30",
      utc("2026-03-29T01:30"),
      "Europe/Berlin",
      "2026-03-29T03:30",
    ],
    ["now in a zone without a change", "2026-10-25T01:30", utc("2026-10-25T01:30"), "UTC", "2026-10-25T01:30"],
    ["now changed in the field", "2026-10-25T03:30", utc("2026-10-25T01:30"), "Europe/Berlin", "2026-10-25T03:30"],
    ["now, in a zone that is none", "2026-10-25T02:30", utc("2026-10-25T01:30"), "Nowhere/Else", "2026-10-25T02:30"],
  ];
  for (const [name, shown, now, zone, expected] of nows)
    test(name, () => {
      expect(sentTime(shown, undefined, now, zone)).toBe(expected);
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

test("sections: the trip in its own order, zone and via apart, then options and advanced in spec order", () => {
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
  expect(names(s.trip)).toEqual(["from", "to", "at"]);
  expect([s.zone?.name, s.via?.name]).toEqual(["time_zone", "via"]);
  expect(names(s.options)).toEqual(["max_changes", "window", "metric"]);
  expect(names(s.advanced)).toEqual(["parallel", "verbose"]);
});

test("sections: a trip field given twice is shown once, the first", () => {
  const s = sections([f("from", "station", "A"), f("from", "station", "B")]);
  expect(s.trip.map((x) => x.default)).toEqual(["A"]);
  expect([...s.options, ...s.advanced]).toEqual([]);
  expect([s.zone, s.via]).toEqual([undefined, undefined]);
});

describe("changed", () => {
  const fields = [f("parallel", "number", "2", true), f("max_changes", "number", "3"), f("planned", "flag", "false")];
  const cases: [string, Record<string, string>, number][] = [
    ["nothing given", {}, 0],
    ["at the defaults", { parallel: "2", max_changes: "3" }, 0],
    ["one changed", { parallel: "4", max_changes: "3" }, 1],
    ["a flag set", { planned: "on" }, 1],
    ["blank is the default", { parallel: "" }, 0],
    ["other fields don't count", { metric: "changes", constructor: "x" }, 0],
    ["all changed", { parallel: "4", max_changes: "5", planned: "on" }, 3],
  ];
  for (const [name, values, expected] of cases)
    test(name, () => {
      expect(changed(fields, values)).toBe(expected);
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

describe("decodeSuggestions", () => {
  test("names with their ids and areas, if known", () => {
    const json =
      '[{"name": "Markaryd", "id": "se:1", "area": "Ödeshögs kommun"}, {"name": "Fulda", "id": "de:1", "area": null}]';
    expect(decodeSuggestions(parseJson(json), "stops")).toEqual([
      { name: "Markaryd", id: "se:1", area: "Ödeshögs kommun" },
      { name: "Fulda", id: "de:1", area: null },
    ]);
  });
  test("an id is required, an area is a string or null", () => {
    expect(() => decodeSuggestions(parseJson('[{"name": "Fulda", "area": null}]'), "stops")).toThrow("stops[0].id");
    expect(() => decodeSuggestions(parseJson('[{"name": "Fulda", "id": "d", "area": 7}]'), "stops")).toThrow(
      "stops[0].area",
    );
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

describe("nextIndex", () => {
  test("the first move goes to either end", () => {
    expect([nextIndex(-1, 3, 1), nextIndex(-1, 3, -1)]).toEqual([0, 2]);
  });
  test("moves wrap around", () => {
    expect([nextIndex(0, 3, 1), nextIndex(2, 3, 1), nextIndex(0, 3, -1), nextIndex(1, 3, -1)]).toEqual([1, 0, 2, 0]);
  });
  test("nothing to highlight in an empty list", () => {
    expect([nextIndex(-1, 0, 1), nextIndex(-1, 0, -1)]).toEqual([-1, -1]);
  });
});
