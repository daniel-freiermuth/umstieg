import { describe, expect, test } from "bun:test";
import { parseJson } from "./decode";
import { type Status, decodeStatus, next } from "./status";

const status = (text: string): Status => decodeStatus(parseJson(text), "status");

describe("decodeStatus", () => {
  test("a running search", () => {
    expect(
      status('{"log": ["rough routes"], "now": "3 requests to DB", "done": false, "error": null, "back": "/?from=A"}'),
    ).toEqual({ log: ["rough routes"], now: "3 requests to DB", done: false, error: null, back: "/?from=A" });
  });

  test("rejects malformed status by path", () => {
    const cases: [string, string][] = [
      ['{"log": "x", "now": null, "done": false, "error": null, "back": ""}', "status.log: expected an array"],
      ['{"log": [1], "now": null, "done": false, "error": null, "back": ""}', "status.log[0]: expected a string"],
      ['{"log": [], "now": null, "done": "yes", "error": null, "back": ""}', "status.done: expected a boolean"],
      ['{"log": [], "now": 3, "done": false, "error": null, "back": ""}', "status.now: expected a string"],
      ['{"log": [], "now": null, "done": true, "error": 5, "back": ""}', "status.error: expected a string"],
      ['{"log": [], "now": null, "done": true, "error": null}', "status.back: expected a value"],
      ["[]", "status: expected an object"],
    ];
    for (const [text, message] of cases) expect(() => status(text)).toThrow(message);
  });
});

describe("next", () => {
  test("polls while running, even with an error already set", () => {
    expect(next({ log: [], now: null, done: false, error: null, back: "" })).toEqual({ poll: true });
    expect(next({ log: [], now: null, done: false, error: "late", back: "" })).toEqual({ poll: true });
  });

  test("opens the plot when done without error", () => {
    expect(next({ log: [], now: null, done: true, error: null, back: "" })).toEqual({ plot: true });
  });

  test("shows why there is no plot, even an empty reason", () => {
    expect(next({ log: [], now: null, done: true, error: "No connection found", back: "" })).toEqual({
      error: "No connection found",
    });
    expect(next({ log: [], now: null, done: true, error: "", back: "" })).toEqual({ error: "" });
  });
});
