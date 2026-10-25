import { describe, expect, test } from "bun:test";
import { parseJson } from "./decode";
import { type Status, decodeStatus, next } from "./status";

const status = (text: string): Status => decodeStatus(parseJson(text), "status");

describe("decodeStatus", () => {
  test("a running search", () => {
    expect(
      status('{"log": ["rough routes", "boards 3/40"], "done": false, "error": null, "back": "/?from=A"}'),
    ).toEqual({ log: ["rough routes", "boards 3/40"], done: false, error: null, back: "/?from=A" });
  });

  test("rejects malformed status by path", () => {
    const cases: [string, string][] = [
      ['{"log": "x", "done": false, "error": null, "back": ""}', "status.log: expected an array"],
      ['{"log": [1], "done": false, "error": null, "back": ""}', "status.log[0]: expected a string"],
      ['{"log": [], "done": "yes", "error": null, "back": ""}', "status.done: expected a boolean"],
      ['{"log": [], "done": true, "error": 5, "back": ""}', "status.error: expected a string"],
      ['{"log": [], "done": true, "error": null}', "status.back: expected a value"],
      ["[]", "status: expected an object"],
    ];
    for (const [text, message] of cases) expect(() => status(text)).toThrow(message);
  });
});

describe("next", () => {
  test("polls while running, even with an error already set", () => {
    expect(next({ log: [], done: false, error: null, back: "" })).toEqual({ poll: true });
    expect(next({ log: [], done: false, error: "late", back: "" })).toEqual({ poll: true });
  });

  test("opens the plot when done without error", () => {
    expect(next({ log: [], done: true, error: null, back: "" })).toEqual({ plot: true });
  });

  test("shows why there is no plot, even an empty reason", () => {
    expect(next({ log: [], done: true, error: "No connection found", back: "" })).toEqual({
      error: "No connection found",
    });
    expect(next({ log: [], done: true, error: "", back: "" })).toEqual({ error: "" });
  });
});
