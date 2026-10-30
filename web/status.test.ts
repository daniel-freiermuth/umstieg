import { describe, expect, test } from "bun:test";
import { parseJson } from "./decode";
import { type Status, decodeProgress, decodeStatus, next } from "./status";

const status = (text: string): Status => decodeStatus(parseJson(text), "status");

describe("decodeStatus", () => {
  test("a running search", () => {
    expect(
      status(
        '{"log": ["rough routes"], "now": "3 requests to DB", "done": false, "error": null, "no_connection": null, "back": "/?from=A"}',
      ),
    ).toEqual({
      log: ["rough routes"],
      now: "3 requests to DB",
      done: false,
      error: null,
      no_connection: null,
      back: "/?from=A",
    });
  });

  test("rejects malformed status by path", () => {
    const cases: [string, string][] = [
      [
        '{"log": "x", "now": null, "done": false, "error": null, "no_connection": null, "back": ""}',
        "status.log: expected an array",
      ],
      [
        '{"log": [1], "now": null, "done": false, "error": null, "no_connection": null, "back": ""}',
        "status.log[0]: expected a string",
      ],
      [
        '{"log": [], "now": null, "done": "yes", "error": null, "no_connection": null, "back": ""}',
        "status.done: expected a boolean",
      ],
      [
        '{"log": [], "now": 3, "done": false, "error": null, "no_connection": null, "back": ""}',
        "status.now: expected a string",
      ],
      [
        '{"log": [], "now": null, "done": true, "error": 5, "no_connection": null, "back": ""}',
        "status.error: expected a string",
      ],
      [
        '{"log": [], "now": null, "done": true, "error": null, "no_connection": 1, "back": ""}',
        "status.no_connection: expected a string",
      ],
      ['{"log": [], "now": null, "done": true, "error": null, "no_connection": null}', "status.back: expected a value"],
      ["[]", "status: expected an object"],
    ];
    for (const [text, message] of cases) expect(() => status(text)).toThrow(message);
  });
});

describe("next", () => {
  test("polls while running, even with an error already set", () => {
    expect(next({ log: [], now: null, done: false, error: null, no_connection: null, back: "" })).toEqual({
      poll: true,
    });
    expect(next({ log: [], now: null, done: false, error: "late", no_connection: null, back: "" })).toEqual({
      poll: true,
    });
  });

  test("opens the plot when done without error", () => {
    expect(next({ log: [], now: null, done: true, error: null, no_connection: null, back: "" })).toEqual({
      plot: true,
    });
  });

  test("shows why there is no plot, even an empty reason", () => {
    expect(
      next({ log: [], now: null, done: true, error: "no stop named like A", no_connection: null, back: "" }),
    ).toEqual({
      error: "no stop named like A",
    });
    expect(next({ log: [], now: null, done: true, error: "", no_connection: null, back: "" })).toEqual({ error: "" });
  });

  test("shows the report of a search that found no connection, apart from failures", () => {
    const report = "rough routes via B\n\nNo connection found.\n";
    expect(next({ log: [], now: null, done: true, error: null, no_connection: report, back: "" })).toEqual({
      noConnection: report,
    });
    expect(next({ log: [], now: null, done: true, error: null, no_connection: "", back: "" })).toEqual({
      noConnection: "",
    });
  });
});

test("the progress page is told where to poll", () => {
  expect(decodeProgress(parseJson('{"status": "/job/7/status"}'), "data")).toEqual({ status: "/job/7/status" });
  expect(() => decodeProgress(parseJson("{}"), "data")).toThrow("data.status");
});
