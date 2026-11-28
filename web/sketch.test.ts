import { describe, expect, test } from "bun:test";
import { parseJson } from "./decode";
import {
  type Ends,
  type Item,
  type Place,
  type Station,
  type Stop,
  addStop,
  apply,
  axis,
  corners,
  decodeItem,
  emptyPicture,
  km,
  nearest,
  spread,
  unit,
} from "./sketch";

const decode = (text: string): Item => decodeItem(parseJson(text), "item");
const mannheim: Place = { name: "Mannheim Hbf", at: [49.47935, 8.46895] };
const kobenhavn: Place = { name: "København H", at: [55.6727, 12.56497] };

describe("decodeItem", () => {
  test("each kind, as the server sends it", () => {
    expect(
      decode(
        '{"kind":"ends","from":{"name":"Mannheim Hbf","at":[49.47935,8.46895]},' +
          '"to":{"name":"København H","at":[55.6727,12.56497]},"depart_from":100,"depart_until":200}',
      ),
    ).toEqual({ kind: "ends", from: mannheim, to: kobenhavn, depart_from: 100, depart_until: 200 });
    expect(
      decode(
        '{"kind":"asked","place":{"name":"Mannheim Hbf","at":[49.47935,8.46895]},"span":[100,3700],' +
          '"seen":[["db:1",null,160],["db:2",300,360]]}',
      ),
    ).toEqual({
      kind: "asked",
      place: mannheim,
      span: [100, 3700],
      seen: [
        ["db:1", null, 160],
        ["db:2", 300, 360],
      ],
    });
    expect(decode('{"kind":"asked","place":{"name":"A","at":[1,2]},"span":null,"seen":[]}')).toEqual({
      kind: "asked",
      place: { name: "A", at: [1, 2] },
      span: null,
      seen: [],
    });
    expect(decode('{"kind":"train","id":"t1","stops":[[1,2,null,60],[3,4,120,null]]}')).toEqual({
      kind: "train",
      id: "t1",
      stops: [
        { at: [1, 2], arrival: null, departure: 60 },
        { at: [3, 4], arrival: 120, departure: null },
      ],
    });
    expect(
      decode(
        '{"kind":"timetable","stations":[{"name":"A","at":[1,2],"height":30},{"name":"B","at":null,"height":null}],' +
          '"trains":[[[0,null,60],[1,120,null]]]}',
      ),
    ).toEqual({
      kind: "timetable",
      stations: [
        { name: "A", at: [1, 2], height: 30 },
        { name: "B", at: null, height: null },
      ],
      trains: [
        [
          [0, null, 60],
          [1, 120, null],
        ],
      ],
    });
    expect(decode('{"kind":"asked_train","trip":5}')).toEqual({ kind: "asked_train", trip: 5 });
  });

  test("rejects malformed items by path", () => {
    const place = '{"name":"A","at":[1,2]}';
    const cases: [string, string][] = [
      ['{"kind":"plan"}', "item.kind: expected one of ends, asked, train, timetable, asked_train"],
      [`{"kind":"ends","from":${place},"to":${place},"depart_from":1}`, "item.depart_until: expected a value"],
      [`{"kind":"ends","from":{"name":"A","at":[1]},"to":${place}}`, "item.from.at: expected a pair"],
      [`{"kind":"asked","place":${place},"span":[1],"seen":[]}`, "item.span: expected a pair"],
      [`{"kind":"asked","place":${place},"span":null,"seen":[["a",1]]}`, "item.seen[0]: expected 3 values"],
      [`{"kind":"asked","place":${place},"span":null,"seen":[[1,1,1]]}`, "item.seen[0][0]: expected a string"],
      [`{"kind":"asked","place":${place},"span":null,"seen":[["a","1",1]]}`, "item.seen[0][1]: expected a number"],
      [`{"kind":"asked","place":${place},"span":null,"seen":[["a",1,"1"]]}`, "item.seen[0][2]: expected a number"],
      ['{"kind":"train","id":"t","stops":[[1,2,3]]}', "item.stops[0]: expected 4 values"],
      ['{"kind":"train","id":"t","stops":[["1",2,3,4]]}', "item.stops[0][0]: expected a number"],
      ['{"kind":"train","id":"t","stops":[[1,"2",3,4]]}', "item.stops[0][1]: expected a number"],
      ['{"kind":"train","id":"t","stops":[[1,2,"3",4]]}', "item.stops[0][2]: expected a number"],
      ['{"kind":"train","id":"t","stops":[[1,2,3,"4"]]}', "item.stops[0][3]: expected a number"],
      ['{"kind":"train","id":"t","stops":{}}', "item.stops: expected an array"],
      ['{"kind":"timetable","stations":[],"trains":[[[0,1]]]}', "item.trains[0][0]: expected 3 values"],
      ['{"kind":"timetable","stations":[],"trains":[[["0",1,1]]]}', "item.trains[0][0][0]: expected a number"],
      ['{"kind":"timetable","stations":[],"trains":[[[0,"1",1]]]}', "item.trains[0][0][1]: expected a number"],
      ['{"kind":"timetable","stations":[],"trains":[[[0,1,"1"]]]}', "item.trains[0][0][2]: expected a number"],
      ['{"kind":"timetable","stations":[{"name":"A","at":null}],"trains":[]}', "item.stations[0].height"],
      ['{"kind":"timetable","stations":[{"name":"A","at":[1],"height":1}],"trains":[]}', "item.stations[0].at"],
      ['{"kind":"asked_train","trip":"5"}', "item.trip: expected a number"],
      ["[]", "item: expected an object"],
    ];
    for (const [text, message] of cases) expect(() => decode(text)).toThrow(message);
  });
});

const stop = (at: [number, number], arrival: number | null, departure: number | null): Stop => ({
  at,
  arrival,
  departure,
});

describe("addStop", () => {
  test("keeps a stop per position in time order, by arrival, else departure", () => {
    const stops: Stop[] = [];
    addStop(stops, stop([2, 2], 200, 210));
    addStop(stops, stop([1, 1], null, 100));
    addStop(stops, stop([4, 4], 400, null));
    addStop(stops, stop([3, 3], 300, 250));
    expect(stops).toEqual([
      stop([1, 1], null, 100),
      stop([2, 2], 200, 210),
      stop([3, 3], 300, 250),
      stop([4, 4], 400, null),
    ]);
  });

  test("a stop at the same time as another comes after it", () => {
    const stops: Stop[] = [stop([1, 1], 100, null)];
    addStop(stops, stop([2, 2], 100, null));
    expect(stops.map((s) => s.at)).toEqual([
      [1, 1],
      [2, 2],
    ]);
  });

  test("at a known position, fills in the times the stop lacks and keeps the rest", () => {
    const stops: Stop[] = [stop([1, 1], null, 100), stop([2, 2], 200, null)];
    addStop(stops, stop([1, 1], 90, 130));
    addStop(stops, stop([2, 2], 250, 260));
    expect(stops).toEqual([stop([1, 1], 90, 100), stop([2, 2], 200, 260)]);
    // Same latitude or same longitude alone is another place.
    addStop(stops, stop([1, 5], 300, null));
    addStop(stops, stop([5, 1], 400, null));
    expect(stops).toHaveLength(4);
  });

  test("leaves out a stop without times, and copies what it keeps", () => {
    const stops: Stop[] = [];
    addStop(stops, stop([1, 1], null, null));
    expect(stops).toEqual([]);
    const added = stop([1, 1], null, 100);
    addStop(stops, added);
    addStop(stops, stop([1, 1], 50, null));
    expect(added.arrival).toBeNull();
  });
});

const ends: Ends = { from: mannheim, to: kobenhavn, depart_from: 100, depart_until: 200 };

describe("apply", () => {
  test("the ends and departure window", () => {
    const picture = emptyPicture();
    expect(apply(picture, { kind: "ends", ...ends })).toBeNull();
    expect(picture.ends).toEqual(ends);
  });

  test("trains add up from train items and what asked stations list", () => {
    const picture = emptyPicture();
    const place: Place = { name: "A", at: [1, 1] };
    expect(apply(picture, { kind: "train", id: "t", stops: [stop([2, 2], 200, null)] })).toBeNull();
    expect(
      apply(picture, {
        kind: "asked",
        place,
        span: [0, 3600],
        seen: [
          ["t", null, 100],
          ["u", 50, 60],
        ],
      }),
    ).toEqual({
      asked: place,
      span: [0, 3600],
    });
    expect([...picture.trains]).toEqual([
      ["t", [stop([1, 1], null, 100), stop([2, 2], 200, null)]],
      ["u", [stop([1, 1], 50, 60)]],
    ]);
  });

  test("the timetable replaces the trains, and later ones are left out; stations are still circled", () => {
    const picture = emptyPicture();
    apply(picture, { kind: "train", id: "t", stops: [stop([2, 2], 200, null)] });
    const timetable = { stations: [{ name: "A", at: null, height: 3 }], trains: [[[0, 1, 2]]] } satisfies {
      stations: Station[];
      trains: [number, number | null, number | null][][];
    };
    expect(apply(picture, { kind: "timetable", ...timetable })).toEqual({ timetable: true });
    expect(picture.timetable).toEqual(timetable);
    expect(picture.trains.size).toBe(0);
    const place: Place = { name: "A", at: [1, 1] };
    apply(picture, { kind: "train", id: "t", stops: [stop([2, 2], 200, null)] });
    expect(apply(picture, { kind: "asked", place, span: null, seen: [["t", 1, 2]] })).toEqual({
      asked: place,
      span: null,
    });
    expect(picture.trains.size).toBe(0);
  });

  test("a timetable train asked about", () => {
    expect(apply(emptyPicture(), { kind: "asked_train", trip: 5 })).toEqual({ askedTrain: 5 });
  });
});

describe("km", () => {
  test("great-circle distances", () => {
    expect(km([0, 0], [0, 1])).toBeCloseTo(111.1949, 3);
    expect(km([60, 0], [60, 1])).toBeCloseTo(55.5969, 3);
    expect(km([50, 8], [51, 10])).toBeCloseTo(179.9181, 3);
    expect(km(mannheim.at, kobenhavn.at)).toBeCloseTo(741.8876, 3);
    expect(km(kobenhavn.at, mannheim.at)).toBeCloseTo(741.8876, 3);
  });
});

describe("axis", () => {
  const line: Ends = { ...ends, from: { name: "S", at: [50, 8] }, to: { name: "G", at: [51, 10] } };
  test("the distance left along the line between the ends", () => {
    expect(axis([50, 8], line)).toBeCloseTo(179.9181, 3);
    expect(axis([51, 10], line)).toBeCloseTo(0, 6);
    expect(axis([50.2, 9.5], line)).toBeCloseTo(82.2267, 3);
  });
  test("ends at one place leave nothing", () => {
    expect(axis([52, 9], { ...line, to: line.from })).toBe(0);
  });
});

describe("spread", () => {
  const values = Array.from({ length: 101 }, (_, i) => 100 - i);
  test("cuts the share at either end", () => {
    expect(spread(values, 0.02, [50, 50])).toEqual([2, 98]);
    expect(spread(values, 0, [50, 50])).toEqual([0, 100]);
    // The share of the gaps between the values, rounded down: 0.45 of 4 gaps is one value cut.
    expect(spread([4, 0, 3, 1, 2], 0.45, [2, 2])).toEqual([1, 3]);
  });
  test("widens to what must be kept", () => {
    expect(spread(values, 0.02, [-5, 120])).toEqual([-5, 120]);
    expect(spread([], 0.02, [3, 4])).toEqual([3, 4]);
  });
  test("leaves the values as they were", () => {
    const mine = [3, 1, 2];
    spread(mine, 0, [2, 2]);
    expect(mine).toEqual([3, 1, 2]);
  });
});

test("unit puts a value into 0..1, an empty range at its middle", () => {
  expect(unit(15, [10, 30])).toBe(0.25);
  expect(unit(7, [7, 7])).toBe(0.5);
});

describe("corners", () => {
  test("arrival and departure at each stop with a height", () => {
    expect(
      corners([
        [10, null, 100],
        [null, 150, 160],
        [5, 200, 210],
        [0, 300, null],
      ]),
    ).toEqual([
      [100, 10],
      [200, 5],
      [210, 5],
      [300, 0],
    ]);
  });
  test("a stop at once arrived and gone is one point", () => {
    expect(
      corners([
        [10, 100, 100],
        [5, 200, null],
      ]),
    ).toEqual([
      [100, 10],
      [200, 5],
    ]);
  });
  test("one point draws nothing", () => {
    expect(
      corners([
        [10, null, 100],
        [null, 200, null],
      ]),
    ).toEqual([]);
  });
});

describe("nearest", () => {
  const stations: Station[] = [
    { name: "far", at: [50, 8], height: 1 },
    { name: "none", at: null, height: 1 },
    { name: "near", at: [50.001, 8], height: 1 },
  ];
  test("the nearest station within the distance", () => {
    expect(nearest(stations, [50.0012, 8], 1)).toBe(2);
    expect(nearest(stations, [49.9999, 8], 1)).toBe(0);
  });
  test("none that near", () => {
    expect(nearest(stations, [50.5, 8], 1)).toBeNull();
    expect(nearest(stations, [50.005, 8], 0.4)).toBeNull();
  });
});
