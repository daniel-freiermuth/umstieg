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
  localDateTime,
  omitted,
  sections,
  stationQuery,
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
      "spec.fields[1].kind: expected one of text, number, flag, choice, time, station, stations",
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
  const now = new Date(2026, 0, 2, 3, 4);
  const cases: [string, Field, Record<string, string>, string | boolean][] = [
    ["a flag is off unless given", f("all_stops", "flag", "true"), {}, false],
    ["a given flag is on", f("all_stops", "flag"), { all_stops: "" }, true],
    ["a time not given is now", f("at", "time", "2026-10-25T08:00"), {}, "2026-01-02T03:04"],
    ["a given time", f("at", "time"), { at: "2026-10-25T08:00" }, "2026-10-25T08:00"],
    ["the default", f("max_changes", "number", "3"), {}, "3"],
    ["a given value over the default", f("max_changes", "number", "3"), { max_changes: "5" }, "5"],
    ["an empty given value over the default", f("via", "stations", "Fulda"), { via: "" }, ""],
  ];
  for (const [name, field, values, expected] of cases)
    test(name, () => {
      expect(initial(field, values, now)).toBe(expected);
    });
});

test("localDateTime pads every part", () => {
  expect(localDateTime(new Date(2026, 8, 5, 7, 3))).toBe("2026-09-05T07:03");
  expect(localDateTime(new Date(2026, 11, 31, 23, 59))).toBe("2026-12-31T23:59");
});

test("sections: the trip in its own order, then limits and advanced in spec order", () => {
  const fields = [
    f("max_changes", "number"),
    f("to", "station"),
    f("parallel", "number", "2", true),
    f("from", "station"),
    f("at", "time"),
    f("metric", "choice"),
    f("verbose", "flag", "", true),
  ];
  const s = sections(fields);
  const names = (xs: Field[]): string[] => xs.map((x) => x.name);
  expect(names(s.trip)).toEqual(["from", "to", "at"]);
  expect(names(s.limits)).toEqual(["max_changes", "metric"]);
  expect(names(s.advanced)).toEqual(["parallel", "verbose"]);
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

describe("stationQuery", () => {
  const cases: [string, boolean, { q: string; prefix: string } | null][] = [
    ["  Mannheim Hbf ", false, { q: "Mannheim Hbf", prefix: "" }],
    ["Ma", false, null],
    // A single station keeps ";" as part of the name.
    ["Fulda; Kas", false, { q: "Fulda; Kas", prefix: "" }],
    ["Fulda; Kas", true, { q: "Kas", prefix: "Fulda; " }],
    [" Fulda ;; Erfurt;  Kas", true, { q: "Kas", prefix: "Fulda; Erfurt; " }],
    ["Fulda; Ka", true, null],
    ["Fulda;", true, null],
  ];
  for (const [value, several, expected] of cases)
    test(`${JSON.stringify(value)}${several ? " (several)" : ""}`, () => {
      expect(stationQuery(value, several)).toEqual(expected);
    });
});
