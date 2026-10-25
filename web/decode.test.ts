import { describe, expect, test } from "bun:test";
import {
  type Decoder,
  type Json,
  DecodeError,
  array,
  boolean,
  field,
  nullable,
  number,
  object,
  oneOf,
  pair,
  parseJson,
  record,
  string,
} from "./decode";

// Every kind of JSON value, to check each decoder rejects all but its own.
const samples: Record<string, Json> = {
  string: "x",
  emptyString: "",
  number: 0,
  boolean: false,
  null: null,
  array: [],
  object: {},
};

function failure(decode: () => void): string {
  try {
    decode();
  } catch (e) {
    expect(e).toBeInstanceOf(DecodeError);
    return e instanceof Error ? e.message : "";
  }
  throw new Error("decoded without error");
}

describe("parseJson", () => {
  test("keeps nested values as parsed", () => {
    expect(parseJson('{"a": [1, "b", null, true, {"c": -2.5}], "d": {}}')).toEqual({
      a: [1, "b", null, true, { c: -2.5 }],
      d: {},
    });
  });

  test("rejects text that is not JSON", () => {
    for (const text of ["", "undefined", "NaN", "{a: 1}", "[1,]", "'x'"])
      expect(() => parseJson(text)).toThrow(SyntaxError);
  });

  test("keeps a __proto__ key as data, not as the object's prototype", () => {
    // JSON.parse makes "__proto__" an own key; copying it by assignment would
    // set the prototype instead, and fields would be read through it.
    const o = object(parseJson('{"__proto__": {"title": "x"}}'), "data");
    expect(Object.keys(o)).toEqual(["__proto__"]);
    expect(failure(() => field(o, "title", string, "data"))).toBe("data.title: expected a value");
  });
});

describe("primitive decoders", () => {
  const cases: [string, Decoder<string | number | boolean>, string[], string][] = [
    ["string", string, ["string", "emptyString"], "a string"],
    ["number", number, ["number"], "a number"],
    ["boolean", boolean, ["boolean"], "a boolean"],
  ];
  for (const [name, decode, accepted, expected] of cases)
    test(`${name} accepts only its own kind`, () => {
      for (const [kind, value] of Object.entries(samples)) {
        if (accepted.includes(kind)) expect<Json>(decode(value, "$")).toBe(value);
        else expect(failure(() => decode(value, "$.x"))).toBe(`$.x: expected ${expected}`);
      }
    });

  test("number does not parse numeric strings", () => {
    expect(failure(() => number("1", "$"))).toBe("$: expected a number");
  });
});

describe("combinators", () => {
  test("nullable passes null and decodes anything else", () => {
    expect(nullable(number)(null, "$")).toBeNull();
    expect(nullable(number)(3, "$")).toBe(3);
    expect(failure(() => nullable(number)("3", "$.n"))).toBe("$.n: expected a number");
  });

  test("array decodes every element and names the failing index", () => {
    expect(array(number)([1, 2, 3], "$")).toEqual([1, 2, 3]);
    expect(array(number)([], "$")).toEqual([]);
    expect(failure(() => array(number)([1, 2, "3"], "$.xs"))).toBe("$.xs[2]: expected a number");
    expect(failure(() => array(number)({ 0: 1 }, "$.xs"))).toBe("$.xs: expected an array");
  });

  test("pair takes exactly two elements", () => {
    const p = pair(number, string);
    expect(p([1, "a"], "$")).toEqual([1, "a"]);
    for (const v of [[], [1], [1, "a", 2], { 0: 1, 1: "a" }, null])
      expect(failure(() => p(v, "$.p"))).toBe("$.p: expected a pair");
    expect(failure(() => p(["a", "a"], "$.p"))).toBe("$.p[0]: expected a number");
    expect(failure(() => p([1, 1], "$.p"))).toBe("$.p[1]: expected a string");
  });

  test("oneOf accepts only the listed strings", () => {
    const kind = oneOf("ride", "wait");
    expect(kind("wait", "$")).toBe("wait");
    for (const v of ["walk", "Ride", "", 0, null])
      expect(failure(() => kind(v, "$.kind"))).toBe("$.kind: expected one of ride, wait");
  });

  test("object rejects arrays and null", () => {
    expect(object({ a: 1 }, "$")).toEqual({ a: 1 });
    for (const v of [[], null, "{}"]) expect(failure(() => object(v, "$.o"))).toBe("$.o: expected an object");
  });

  test("field fails on missing fields, by path", () => {
    const o = object(parseJson('{"a": 1, "b": null}'), "$");
    expect(field(o, "a", number, "$")).toBe(1);
    expect(field(o, "b", nullable(number), "$")).toBeNull();
    expect(failure(() => field(o, "c", number, "$.x"))).toBe("$.x.c: expected a value");
    // Names of Object.prototype members are not fields.
    for (const name of ["constructor", "toString", "hasOwnProperty"])
      expect(failure(() => field(o, name, string, "$"))).toBe(`$.${name}: expected a value`);
  });

  test("record decodes every value, keyed by path", () => {
    expect(record(string)({ from: "Mannheim", to: "Berlin" }, "$")).toEqual({ from: "Mannheim", to: "Berlin" });
    expect(failure(() => record(string)({ from: "Mannheim", at: 8 }, "$.values"))).toBe(
      "$.values.at: expected a string",
    );
    expect(failure(() => record(string)(["a"], "$.values"))).toBe("$.values: expected an object");
  });

  test("record keeps __proto__ as an own key", () => {
    const values = record(string)(parseJson('{"__proto__": "x", "a": "b"}'), "$");
    expect(Object.keys(values)).toEqual(["__proto__", "a"]);
    expect(Object.getPrototypeOf(values)).toBe(Object.prototype);
  });

  test("nested decoders report the full path", () => {
    const decode = (v: Json, path: string): number[][] => {
      const o = object(v, path);
      return field(o, "rows", array(array(number)), path);
    };
    expect(failure(() => decode(parseJson('{"rows": [[1], [2, true]]}'), "data"))).toBe(
      "data.rows[1][1]: expected a number",
    );
  });
});
