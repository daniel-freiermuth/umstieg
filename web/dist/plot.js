// decode.ts
class DecodeError extends Error {
}
function fail(path, expected) {
  throw new DecodeError(`${path}: expected ${expected}`);
}
function toJson(value) {
  if (value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return value;
  }
  if (Array.isArray(value))
    return value.map((x) => toJson(x));
  if (typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, x]) => [key, toJson(x)]));
  }
  return fail("$", "JSON");
}
function parseJson(text) {
  const value = JSON.parse(text);
  return toJson(value);
}
var isArray = (value) => Array.isArray(value);
var isObject = (value) => typeof value === "object" && value !== null && !isArray(value);
var string = (v, path) => typeof v === "string" ? v : fail(path, "a string");
var number = (v, path) => typeof v === "number" ? v : fail(path, "a number");
var boolean = (v, path) => typeof v === "boolean" ? v : fail(path, "a boolean");
var nullable = (decode) => (v, path) => v === null ? null : decode(v, path);
var array = (decode) => (v, path) => isArray(v) ? v.map((x, i) => decode(x, `${path}[${String(i)}]`)) : fail(path, "an array");
var pair = (first, second) => (v, path) => {
  if (!isArray(v) || v.length !== 2)
    return fail(path, "a pair");
  const [a, b] = v;
  if (a === undefined || b === undefined)
    return fail(path, "a pair");
  return [first(a, `${path}[0]`), second(b, `${path}[1]`)];
};
var oneOf = (...options) => (v, path) => options.find((o) => o === v) ?? fail(path, `one of ${options.join(", ")}`);
var object = (v, path) => isObject(v) ? v : fail(path, "an object");
function field(o, name, decode, path) {
  const v = Object.hasOwn(o, name) ? o[name] : undefined;
  return v === undefined ? fail(`${path}.${name}`, "a value") : decode(v, `${path}.${name}`);
}
var record = (decode) => (v, path) => {
  const o = object(v, path);
  return Object.fromEntries(Object.entries(o).map(([key, x]) => [key, decode(x, `${path}.${key}`)]));
};

// util.ts
function item(xs, i) {
  const x = xs[i];
  if (x === undefined)
    throw new Error(`index ${String(i)} out of range`);
  return x;
}
function entry(m, k, make) {
  let v = m.get(k);
  if (v === undefined) {
    v = make();
    m.set(k, v);
  }
  return v;
}

// data.ts
var kinds = ["ride", "wait", "walk"];
var decodeMetric = (v, path) => {
  const o = object(v, path);
  return {
    id: field(o, "id", string, path),
    unit: field(o, "unit", string, path),
    description: field(o, "description", string, path),
    heights: field(o, "heights", array(nullable(number)), path)
  };
};
var decodePiece = (v, path) => {
  const o = object(v, path);
  return {
    kind: field(o, "kind", oneOf(...kinds), path),
    info: field(o, "info", string, path),
    warn: field(o, "warn", boolean, path),
    trip: field(o, "trip", nullable(number), path),
    pts: field(o, "pts", array(pair(number, number)), path)
  };
};
var decodeDetail = (v, path) => {
  const o = object(v, path);
  return {
    route: field(o, "route", string, path),
    fallbacks: field(o, "fallbacks", array(pair(number, string)), path)
  };
};
var decodeDetails = array(decodeDetail);
var decodeData = (v, path) => {
  const o = object(v, path);
  return {
    title: field(o, "title", string, path),
    subtitle: field(o, "subtitle", string, path),
    back: field(o, "back", nullable(string), path),
    tz: field(o, "tz", string, path),
    metrics: field(o, "metrics", array(decodeMetric), path),
    metric: field(o, "metric", number, path),
    stations: field(o, "stations", array(number), path),
    names: field(o, "names", array(string), path),
    trips: field(o, "trips", array(string), path),
    groups: field(o, "groups", array((g, p) => {
      const og = object(g, p);
      return { label: field(og, "label", string, p), count: field(og, "count", number, p) };
    }), path),
    pieces: field(o, "pieces", array(decodePiece), path),
    network: field(o, "network", array(number), path),
    fallbacks: field(o, "fallbacks", array((f, p) => {
      const of = object(f, p);
      return {
        at: field(of, "at", string, p),
        line: field(of, "line", string, p),
        pieces: field(of, "pieces", array(number), p)
      };
    }), path),
    chains: field(o, "chains", array((c, p) => {
      const oc = object(c, p);
      return {
        group: field(oc, "group", number, p),
        header: field(oc, "header", string, p),
        pieces: field(oc, "pieces", array(number), p)
      };
    }), path),
    details: field(o, "details", nullable(decodeDetails), path),
    details_url: field(o, "details_url", nullable(string), path)
  };
};
function load(data) {
  const { pieces: rawPieces, fallbacks: rawFallbacks, chains, details: rawDetails, ...rest } = data;
  const pieces = rawPieces.map(({ kind, info, warn, trip, pts }) => ({
    kind,
    info,
    warn,
    trip,
    raw: pts.map(([m, s]) => [m * 60000, s]),
    pts: []
  }));
  const fallbacks = rawFallbacks.map((f) => ({ at: f.at, line: f.line, pieces: f.pieces.map((i) => item(pieces, i)) }));
  const plot = { ...rest, pieces, fallbacks, chains, details: new Map };
  addDetails(plot, (rawDetails ?? []).map((d, i) => [i, d]));
  return plot;
}
function addDetails(plot, details) {
  for (const [i, { route, fallbacks }] of details)
    plot.details.set(i, {
      route,
      fallbacks: fallbacks.map(([f, arrive]) => ({ ...item(plot.fallbacks, f), arrive }))
    });
}
function applyHeights(plot, metric) {
  const heights = item(plot.metrics, metric).heights;
  for (const p of plot.pieces)
    p.pts = p.raw.flatMap(([t, s]) => {
      const h = heights[s] ?? null;
      return h === null ? [] : [[t, h, s]];
    });
  return plot.stations.flatMap((s) => {
    const h = heights[s] ?? null;
    return h === null ? [] : [{ i: s, name: item(plot.names, s), y: h }];
  }).sort((a, b) => b.y - a.y || a.name.localeCompare(b.name));
}
function extent(plot) {
  const full = { t0: Infinity, t1: -Infinity, y0: 0, y1: 0 };
  for (const i of plot.network)
    for (const [t, v] of item(plot.pieces, i).pts) {
      full.t0 = Math.min(full.t0, t);
      full.t1 = Math.max(full.t1, t);
      full.y1 = Math.max(full.y1, v);
    }
  full.t0 -= 10 * 60000;
  full.t1 += 10 * 60000;
  return full;
}

// dom.ts
function byId(id, type) {
  const element = document.getElementById(id);
  if (!(element instanceof type))
    throw new Error(`#${id} missing`);
  return element;
}

// geometry.ts
function clipLine(pts, from, to) {
  if (from === -Infinity && to === Infinity)
    return pts;
  const at = (a, b, t) => [t, a[1] + (b[1] - a[1]) * (t - a[0]) / (b[0] - a[0]), a[2]];
  const cut = [];
  const push = (q) => {
    const l = cut[cut.length - 1];
    if (l?.[0] !== q[0] || l[1] !== q[1])
      cut.push(q);
  };
  for (let k = 1;k < pts.length; k++) {
    const a = item(pts, k - 1), b = item(pts, k);
    if (b[0] < from || a[0] > to)
      continue;
    if (a[0] === b[0]) {
      push(a);
      push(b);
      continue;
    }
    push(at(a, b, Math.max(a[0], from)));
    push(at(a, b, Math.min(b[0], to)));
  }
  return cut;
}
var MIN_T = 10 * 60000;
function clampDomain(dom, full) {
  let { t0, t1, y0, y1 } = dom;
  const minY = (full.y1 - full.y0) / 200;
  if (t1 - t0 < MIN_T) {
    const c = (t0 + t1) / 2;
    t0 = c - MIN_T / 2;
    t1 = c + MIN_T / 2;
  }
  if (y1 - y0 < minY) {
    const c = (y0 + y1) / 2;
    y0 = c - minY / 2;
    y1 = c + minY / 2;
  }
  const span = (lo, hi, a, b) => {
    if (hi - lo > b - a)
      return [a, b];
    if (lo < a)
      return [a, a + hi - lo];
    if (hi > b)
      return [b - (hi - lo), b];
    return [lo, hi];
  };
  [t0, t1] = span(t0, t1, full.t0, full.t1);
  [y0, y1] = span(y0, y1, full.y0, full.y1);
  return { t0, t1, y0, y1 };
}
function zoom(dom, full, { fx, fy }, k, { time, height }) {
  let { t0, t1, y0, y1 } = dom;
  if (time) {
    const at = t0 + fx * (t1 - t0);
    [t0, t1] = [at - (at - t0) * k, at + (t1 - at) * k];
  }
  if (height) {
    const at = y1 - fy * (y1 - y0);
    [y0, y1] = [at - (at - y0) * k, at + (y1 - at) * k];
  }
  return clampDomain({ t0, t1, y0, y1 }, full);
}
function pan(start, full, dx, dy, { W, H, m }) {
  const dt = dx / (W - m.l - m.r) * (start.t1 - start.t0);
  const dv = dy / (H - m.t - m.b) * (start.y1 - start.y0);
  return clampDomain({ t0: start.t0 - dt, t1: start.t1 - dt, y0: start.y0 + dv, y1: start.y1 + dv }, full);
}
var PINCH_MIN = 40;
function pinch(start, full, from, to, g) {
  const mid = ([a, b]) => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });
  const k = (was, now) => Math.abs(was) < PINCH_MIN || Math.abs(now) < PINCH_MIN ? 1 : Math.abs(was) / Math.abs(now);
  const [m0, m1] = [mid(from), mid(to)];
  const at = { fx: (m0.x - g.m.l) / (g.W - g.m.l - g.m.r), fy: (m0.y - g.m.t) / (g.H - g.m.t - g.m.b) };
  const kt = k(from[1].x - from[0].x, to[1].x - to[0].x);
  const ky = k(from[1].y - from[0].y, to[1].y - to[0].y);
  const zoomed = zoom(zoom(start, full, at, kt, { time: true, height: false }), full, at, ky, {
    time: false,
    height: true
  });
  return pan(zoomed, full, m1.x - m0.x, m1.y - m0.y, g);
}
var TICK_STEPS = [5, 10, 15, 30, 60, 120, 180, 360].map((mins) => mins * 60000);
var TICK_WIDTH = 50;
function tickStep(span, width) {
  const most = Math.min(12, Math.max(2, Math.floor(width / TICK_WIDTH)));
  return TICK_STEPS.find((s) => span / s <= most) ?? item(TICK_STEPS, TICK_STEPS.length - 1);
}
function spreadLabels(ys, gap, bottom) {
  const at = [...ys];
  for (let k = 1;k < at.length; k++)
    at[k] = Math.max(item(at, k), item(at, k - 1) + gap);
  const lastAt = at[at.length - 1];
  if (lastAt !== undefined && lastAt > bottom) {
    at[at.length - 1] = bottom;
    for (let k = at.length - 2;k >= 0; k--)
      at[k] = Math.min(item(at, k), item(at, k + 1) - gap);
  }
  return at;
}
function distanceToLine(px, py, p) {
  let best = Infinity;
  for (let k = 0;k < p.length; k++) {
    const a = item(p, k), b = item(p, Math.min(k + 1, p.length - 1));
    const dx = b.x - a.x, dy = b.y - a.y, len = dx * dx + dy * dy;
    const u = len ? Math.max(0, Math.min(1, ((px - a.x) * dx + (py - a.y) * dy) / len)) : 0;
    best = Math.min(best, Math.hypot(px - a.x - u * dx, py - a.y - u * dy));
  }
  return best;
}

// reach.ts
function covers(segments, lines, spot) {
  if ("piece" in spot) {
    const { piece, from, to } = spot;
    return segments.some(([pi, a, b]) => pi === piece && (from === to ? a <= from && from <= b : a < to && b > from));
  }
  const { station, t } = spot;
  return segments.some(([pi, a, b]) => {
    if (t < a || t > b)
      return false;
    const pts = item(lines, pi).pts;
    return pts.some((q, k) => {
      const p = pts[k - 1];
      return p?.[2] === station && q[2] === station && p[0] <= t && t <= q[0];
    });
  });
}
function runsOf(spans) {
  return mergeIntervals(spans.map((w) => [w.from, w.to]));
}
function mergeIntervals(list) {
  const runs = [];
  for (const [a, b] of [...list].sort((x, y) => x[0] - y[0])) {
    const last = runs[runs.length - 1];
    if (last && a <= last[1])
      last[1] = Math.max(last[1], b);
    else
      runs.push([a, b]);
  }
  return runs;
}
function stayAt(spans, from, to, time, y, s) {
  const t = Math.min(to, Math.max(from, time));
  const here = spans.filter((w) => w.from <= t && t <= w.to);
  return {
    t,
    here,
    pts: [
      [from, y, s],
      [to, y, s]
    ]
  };
}
function intersectReach(segmentLists) {
  const maps = segmentLists.map((list) => {
    const m = new Map;
    for (const [pi, a, b] of list)
      entry(m, pi, () => []).push([a, b]);
    return m;
  });
  const [head, ...rest] = maps;
  if (!head)
    return [];
  const out = [];
  for (const [pi, first] of head) {
    let cur = first;
    for (const m of rest) {
      const other = m.get(pi);
      if (!other) {
        cur = [];
        break;
      }
      const next = [];
      for (const [a, b] of cur)
        for (const [c, d] of other) {
          const lo = Math.max(a, c), hi = Math.min(b, d);
          if (lo <= hi)
            next.push([lo, hi]);
        }
      cur = next;
    }
    for (const [a, b] of cur)
      out.push([pi, a, b]);
  }
  return out;
}
var atMost = (v, t) => v !== undefined && v <= t;
var atLeast = (v, t) => v !== undefined && v >= t;
var stretchKey = (trip, a, b) => `${String(trip)}:${String(a[0])}:${String(a[2])}:${String(b[0])}:${String(b[2])}`;
function network(pieces, base, chains, visible) {
  const rides = new Map, waits = [], walks = new Map, stays = new Map;
  const stay = (s, y, span) => {
    entry(stays, s, () => ({ y, spans: [] })).spans.push(span);
  };
  for (const pi of base) {
    const p = item(pieces, pi);
    if (p.trip !== null) {
      for (let k = 1;k < p.pts.length; k++) {
        const a = item(p.pts, k - 1), b = item(p.pts, k);
        rides.set(stretchKey(p.trip, a, b), { trip: p.trip, pts: [a, b], piece: pi, chains: new Set });
        if (a[2] === b[2])
          stay(a[2], a[1], { from: a[0], to: b[0], trip: p.trip });
      }
    } else if (p.kind === "wait") {
      const first = p.pts[0], last = p.pts[p.pts.length - 1];
      if (!first || !last)
        continue;
      waits.push(pi);
      stay(first[2], first[1], { from: first[0], to: last[0] });
    } else
      walks.set(pi, new Set);
  }
  const present = new Map, touching = new Map;
  chains.forEach((c, i) => {
    if (!visible(i))
      return;
    for (const pi of c.pieces) {
      const p = item(pieces, pi);
      for (const [, , s] of p.pts)
        entry(touching, s, () => new Set).add(i);
      if (p.trip !== null) {
        for (let k = 1;k < p.pts.length; k++) {
          const a = item(p.pts, k - 1), b = item(p.pts, k);
          rides.get(stretchKey(p.trip, a, b))?.chains.add(i);
          if (a[2] === b[2])
            entry(present, a[2], () => []).push({ from: a[0], to: b[0], chain: i });
        }
      } else if (p.kind === "wait") {
        const first = p.pts[0], last = p.pts[p.pts.length - 1];
        if (first && last)
          entry(present, first[2], () => []).push({ from: first[0], to: last[0], chain: i });
      } else
        walks.get(pi)?.add(i);
    }
  });
  const chainsAt = (station, t) => [
    ...new Set((present.get(station) ?? []).filter((w) => w.from <= t && t <= w.to).map((w) => w.chain))
  ];
  const all = [...rides.values()];
  const groups = (key, later) => {
    const by = new Map;
    for (const r of all)
      entry(by, key(r), () => []).push(r);
    return [...by].sort((x, y) => later ? y[0] - x[0] : x[0] - y[0]).map(([, rs]) => rs);
  };
  const byDep = groups((r) => r.pts[0][0], false), byArr = groups((r) => r.pts[1][0], true);
  const foot = new Map, footBack = new Map;
  for (const pi of walks.keys()) {
    const p = item(pieces, pi);
    const a = p.pts[0], b = p.pts[p.pts.length - 1];
    if (!a || !b)
      continue;
    entry(foot, a[2], () => []).push([b[2], b[0] - a[0]]);
    entry(footBack, b[2], () => []).push([a[2], b[0] - a[0]]);
  }
  const reach = (src) => {
    const ea = new Map, ld = new Map;
    const early = (x, t) => {
      if (atMost(ea.get(x), t))
        return;
      ea.set(x, t);
      for (const [y, d] of foot.get(x) ?? [])
        early(y, t + d);
    };
    const late = (x, t) => {
      if (atLeast(ld.get(x), t))
        return;
      ld.set(x, t);
      for (const [y, d] of footBack.get(x) ?? [])
        late(y, t - d);
    };
    for (const [s, t] of src.at) {
      early(s, t);
      late(s, t);
    }
    for (const [s, t] of src.back ?? [])
      late(s, t);
    for (const [s, t] of src.fwd ?? [])
      early(s, t);
    const raw = new Map;
    const add = (pi, a, b) => {
      entry(raw, pi, () => []).push([a, b]);
    };
    const boarded = new Map(src.ride ? [[src.ride.trip, src.ride.from]] : []);
    const ahead = new Set, behind = new Set;
    for (const group of byDep) {
      for (let again = true;again; ) {
        again = false;
        for (const r of group) {
          const [a, b] = r.pts;
          const since = boarded.get(r.trip);
          if (ahead.has(r) || !(since !== undefined && a[0] >= since || atMost(ea.get(a[2]), a[0])))
            continue;
          boarded.set(r.trip, a[0]);
          early(b[2], b[0]);
          ahead.add(r);
          again ||= b[0] === a[0];
        }
      }
    }
    const alighted = new Map(src.ride ? [[src.ride.trip, src.ride.until]] : []);
    for (const group of byArr) {
      for (let again = true;again; ) {
        again = false;
        for (const r of group) {
          const [a, b] = r.pts;
          const until = alighted.get(r.trip);
          if (behind.has(r) || !(until !== undefined && b[0] <= until || atLeast(ld.get(b[2]), b[0])))
            continue;
          alighted.set(r.trip, b[0]);
          late(a[2], a[0]);
          behind.add(r);
          again ||= b[0] === a[0];
        }
      }
    }
    for (const r of new Set([...ahead, ...behind]))
      add(r.piece, r.pts[0][0], r.pts[1][0]);
    for (const pi of waits) {
      const p = item(pieces, pi);
      const first = p.pts[0], last = p.pts[p.pts.length - 1];
      if (!first || !last)
        continue;
      const [from, , s] = first, to = last[0];
      const there = ea.get(s), by = ld.get(s);
      if (there !== undefined && to >= there)
        add(pi, Math.max(from, there), to);
      if (by !== undefined && from <= by)
        add(pi, from, Math.min(to, by));
    }
    for (const pi of walks.keys()) {
      const p = item(pieces, pi);
      const a = p.pts[0], b = p.pts[p.pts.length - 1];
      if (!a || !b)
        continue;
      if (atMost(ea.get(a[2]), a[0]) || atLeast(ld.get(b[2]), b[0]))
        add(pi, a[0], b[0]);
    }
    const out = [];
    for (const [pi, list] of raw)
      for (const [a, b] of mergeIntervals(list))
        out.push([pi, a, b]);
    return out;
  };
  return { rides: all, stays, waits, walks, chainsAt, touching, reach };
}

// picks.ts
function narrow(picks, visible) {
  const [first, ...rest] = picks;
  if (!first)
    return null;
  let chains = first.chains.filter(visible);
  for (const p of rest) {
    const s = new Set(p.chains);
    chains = chains.filter((i) => s.has(i));
  }
  const reaches = picks.map((p) => p.reach).filter((r) => r !== null);
  const reach = reaches.length ? {
    key: picks.map((p) => p.reach?.key ?? p.id).join("|"),
    segments: intersectReach(reaches.map((r) => r.segments))
  } : null;
  return { info: picks.map((p) => p.info).join(" + "), chains, reach };
}
function pinAfter(chains, pinned) {
  if (chains.length === 1)
    return item(chains, 0);
  return pinned !== null && chains.includes(pinned) ? pinned : null;
}
function step(list, pinned, down) {
  const now = pinned === null ? -1 : list.indexOf(pinned);
  const next = now < 0 ? 0 : (now + (down ? 1 : list.length - 1)) % list.length;
  return list[next] ?? null;
}
function toggle(picks, target) {
  return picks.some((p) => p.id === target.id) ? picks.filter((p) => p.id !== target.id) : [...picks, target];
}

// plot.ts
var D = load(decodeData(parseJson(byId("data", HTMLScriptElement).text), "data"));
var DETAILS_AT_ONCE = 500;
var MAX_LISTED = 200;
var asked = new Set;
var lost = new Set;
var routeOf = (i) => D.details.get(i)?.route ?? (lost.has(i) ? "(details gone: the search has expired, run it again)" : "…");
function detailOf(i) {
  const known = D.details.get(i);
  if (known === undefined)
    want([i]);
  return known;
}
function want(chains) {
  const missing = chains.filter((i) => !D.details.has(i) && !asked.has(i));
  for (let at = 0;at < missing.length; at += DETAILS_AT_ONCE) {
    const ids = missing.slice(at, at + DETAILS_AT_ONCE);
    for (const i of ids)
      asked.add(i);
    (async () => {
      try {
        const response = D.details_url === null ? null : await fetch(`${D.details_url}?ids=${ids.join(",")}`);
        if (response?.ok) {
          const got = decodeDetails(parseJson(await response.text()), "details");
          addDetails(D, got.map((d, k) => [item(ids, k), d]));
        } else
          for (const i of ids)
            lost.add(i);
      } catch {
        for (const i of ids)
          asked.delete(i);
        return;
      }
      panelKey = null;
      if (view)
        view.fbKey = undefined;
      refresh();
    })();
  }
}
var NS = "http://www.w3.org/2000/svg";
var plotEl = byId("plot", HTMLDivElement);
var tip = byId("tip", HTMLDivElement);
var details = byId("details", HTMLDivElement);
var panelTitle = byId("panel-title", HTMLHeadingElement);
var picksEl = byId("picks", HTMLDivElement);
var tickFmt = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
  timeZone: D.tz
});
var timeFmt = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
  timeZone: D.tz,
  timeZoneName: "short"
});
var trainColor = (t) => `hsl(${String(t * 137.508 % 360)}, 70%, 45%)`;
var HINT = "Drawn is everything on some connection within the limits; listed are the best connections. Hover a line " + "or station name to see what is reachable through it. Click it to pick it: the listed connections passing " + "it are shown (one alone is selected). Click more to narrow down (hovering previews it; grey parts lead past " + "your picks), a pick again to remove it.";
var hidden = new Set;
var pinned = null;
var picks = [];
var candidates = null;
var hoverTarget = null;
var preview = null;
var softGroup = null;
byId("title", HTMLHeadingElement).textContent = D.title;
byId("subtitle", HTMLParagraphElement).textContent = D.subtitle;
if (D.back)
  Object.assign(byId("new-search", HTMLAnchorElement), { href: D.back, hidden: false });
var legend = byId("legend", HTMLDivElement);
function writeLegend() {
  const metric = item(D.metrics, metricIndex);
  legend.innerHTML = `x: time (${D.tz}). y: ${metric.description} (${metric.unit}), goal at the bottom.<br>` + `Coloured: riding, one colour per train · thin grey: waiting (red: below minimum transfer, or missed by live data ✗) · dotted: walking.<br>` + `Parallel lines: same route later. Steeper: faster. Dashed: fallbacks of the selection (orange dots: waiting).<br>` + `<kbd>↑</kbd> <kbd>↓</kbd> step through listed connections, <kbd>Esc</kbd> clears the selection.<br>` + `Wheel: zoom (<kbd>Shift</kbd> time only, <kbd>Alt</kbd> height only) · drag: pan · two fingers: zoom (sideways time, up and down height) · double-click: reset.`;
}
function fitText(t, text, width) {
  t.textContent = text;
  const whole = t.getComputedTextLength();
  if (whole <= width)
    return;
  let keep = Math.max(1, Math.floor(text.length * width / whole));
  t.textContent = text.slice(0, keep).trimEnd() + "…";
  while (keep > 1 && t.getComputedTextLength() > width)
    t.textContent = text.slice(0, --keep).trimEnd() + "…";
  el("title", {}, t).textContent = text;
}
function el(name, attrs, parent) {
  const e = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs))
    e.setAttribute(k, String(v));
  if (parent)
    parent.appendChild(e);
  return e;
}
var ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" };
function esc(s) {
  return s.replace(/[&<>"]/g, (ch) => ESCAPES[ch] ?? ch);
}
var metricIndex = D.metric;
var rows = [];
var full = { t0: Infinity, t1: -Infinity, y0: 0, y1: 0 };
var dom = { ...full };
var model = null;
var view = null;
var geom = null;
function heightOf(s) {
  return item(D.metrics, metricIndex).heights[s] ?? null;
}
function applyMetric(k) {
  metricIndex = k;
  rows = applyHeights(D, k);
  full = extent(D);
  dom = { ...full };
  writeLegend();
}
var metricSelect = byId("metric", HTMLSelectElement);
D.metrics.forEach((m, k) => {
  const o = document.createElement("option");
  o.value = String(k);
  o.textContent = m.id;
  o.title = `${m.description} (${m.unit})`;
  metricSelect.appendChild(o);
});
metricSelect.value = String(D.metric);
metricSelect.addEventListener("change", () => {
  applyMetric(+metricSelect.value);
  buildModel();
  render();
});
applyMetric(D.metric);
function buildModel() {
  const net = network(D.pieces, D.network, D.chains, (i) => !hidden.has(item(D.chains, i).group));
  const { stays, walks, touching, chainsAt, reach: closure } = net;
  const edges = net.rides.map((r) => {
    const [a, b] = r.pts;
    const info = `${item(D.trips, r.trip)} · ${timeFmt.format(a[0])} ${item(D.names, a[2])} → ${timeFmt.format(b[0])} ${item(D.names, b[2])}`;
    return { ...r, info };
  });
  const targets = [];
  for (const e of edges) {
    if (e.pts[0][2] === e.pts[1][2])
      continue;
    const [a, b] = e.pts;
    const id = `ride:${String(e.trip)}:${String(a[0])}:${String(b[0])}`;
    let t = null;
    targets.push({
      pts: e.pts,
      at: () => {
        t ??= {
          id,
          info: e.info,
          trip: e.trip,
          chains: [...e.chains],
          pts: e.pts,
          spot: { piece: e.piece, from: a[0], to: b[0] },
          reach: {
            key: id,
            with: e.chains,
            segments: closure({ at: [], back: [[a[2], a[0]]], ride: { trip: e.trip, from: a[0], until: a[0] } })
          }
        };
        return t;
      }
    });
  }
  for (const [s, { y, spans }] of stays) {
    let reach = null;
    for (const [from, to] of runsOf(spans))
      targets.push({
        pts: [
          [from, y, s],
          [to, y, s]
        ],
        at: (time) => {
          const { t, here, pts } = stayAt(spans, from, to, time, y, s);
          const trains = [...new Set(here.flatMap((w) => w.trip === undefined ? [] : [w.trip]))];
          const waiting = here.some((w) => w.trip === undefined);
          const parts = [waiting ? "waiting" : null, ...trains.map((tr) => `standing: ${item(D.trips, tr)}`)].filter((x) => x !== null);
          const key = `${String(s)}:${String(spans.filter((w) => w.from <= t).length)}:${String(spans.filter((w) => w.to >= t).length)}`;
          const r = reach?.key === key ? reach : { key, segments: closure({ at: [[s, t]] }) };
          reach = r;
          const trip = trains.length === 1 && !waiting ? trains[0] : undefined;
          return {
            id: `stay:${String(s)}:${String(from)}`,
            info: `at ${item(D.names, s)} · ${timeFmt.format(t)} · ${parts.join(" · ")}`,
            ...trip === undefined ? {} : { trip },
            chains: chainsAt(s, t),
            reach: r,
            pts,
            spot: { station: s, t }
          };
        }
      });
  }
  for (const [pi, chains] of walks) {
    const p = item(D.pieces, pi);
    const [first, last] = [p.pts[0], p.pts[p.pts.length - 1]];
    const spot = first && last ? { piece: pi, from: first[0], to: last[0] } : undefined;
    const t = {
      id: `walk:${String(pi)}`,
      info: p.info,
      chains: [...chains],
      pts: p.pts,
      ...spot ? { spot } : {}
    };
    targets.push({ pts: t.pts, at: () => t });
  }
  const stationTargets = new Map;
  const stationTarget = (s) => entry(stationTargets, s, () => {
    const chains = touching.get(s) ?? new Set;
    const h = heightOf(s);
    if (h === null)
      throw new Error(`station ${String(s)} has no height`);
    return {
      id: `station:${String(s)}`,
      info: `${item(D.names, s)} · any time`,
      chains: [...chains],
      pts: [
        [full.t0, h, s],
        [full.t1, h, s]
      ],
      reach: {
        key: `station:${String(s)}`,
        with: chains,
        segments: closure({ at: [], fwd: [[s, -Infinity]], back: [[s, Infinity]] })
      }
    };
  });
  model = {
    edges,
    waits: net.waits,
    walks: [...walks.keys()],
    targets,
    stationTarget
  };
}
function render() {
  const md = model;
  if (!md)
    return;
  plotEl.querySelectorAll("svg").forEach((s) => {
    s.remove();
  });
  const { clientWidth: W, clientHeight: H } = plotEl;
  const m = { l: Math.min(240, Math.round(W * 0.4)), r: 16, t: 14, b: 28 };
  geom = { W, H, m };
  const x = (t) => m.l + (t - dom.t0) / (dom.t1 - dom.t0) * (W - m.l - m.r);
  const y = (v) => m.t + (dom.y1 - v) / (dom.y1 - dom.y0 || 1) * (H - m.t - m.b);
  const svg = el("svg", { width: W, height: H });
  plotEl.insertBefore(svg, tip);
  const clip = el("clipPath", { id: "area" }, el("defs", {}, svg));
  el("rect", { x: m.l, y: m.t - 6, width: W - m.l - m.r, height: H - m.b - m.t + 6 }, clip);
  const hue = (i) => i * 47 % 360;
  const st = rows.map((s) => ({ ...s, py: y(s.y) })).filter((s) => s.py >= m.t - 1 && s.py <= H - m.b + 1);
  const bands = el("g", { "clip-path": "url(#area)" }, svg);
  st.forEach((s, k) => {
    const prev = st[k - 1], next = st[k + 1];
    const above = prev ? (prev.py + s.py) / 2 : m.t;
    const below = next ? (s.py + next.py) / 2 : H - m.b;
    el("rect", {
      x: m.l,
      y: above,
      width: W - m.l - m.r,
      height: Math.max(0, below - above),
      fill: `hsl(${String(hue(s.i))}, 60%, 50%)`,
      "fill-opacity": 0.05
    }, bands);
  });
  const gap = Math.min(12, (H - m.t - m.b) / Math.max(1, st.length));
  const at = spreadLabels(st.map((s) => s.py), gap, H - m.b);
  const stations = el("g", { class: "station" }, svg);
  st.forEach((s, k) => {
    const ak = item(at, k);
    el("line", { x1: m.l, x2: W - m.r, y1: s.py, y2: s.py }, stations);
    el("polyline", {
      points: `${String(m.l - 26)},${String(ak)} ${String(m.l - 8)},${String(s.py)} ${String(m.l)},${String(s.py)}`,
      class: "leader",
      stroke: `hsl(${String(hue(s.i))}, 60%, 50%)`
    }, stations);
    const t = el("text", { x: m.l - 30, y: ak + gap / 3, "text-anchor": "end", "font-size": Math.min(11, gap), class: "station-name" }, stations);
    fitText(t, s.name, m.l - 34);
    t.addEventListener("mousemove", (e) => {
      if (drag)
        return;
      hoverTarget = md.stationTarget(s.i);
      refresh();
      moveTip(e);
    });
    t.addEventListener("mouseleave", () => {
      hoverTarget = null;
      refresh();
    });
    t.addEventListener("click", (e) => {
      e.stopPropagation();
      if (!dragged)
        choose(md.stationTarget(s.i));
    });
  });
  const axis = el("g", { class: "axis" }, svg);
  const step2 = tickStep(dom.t1 - dom.t0, W - m.l - m.r);
  for (let t = Math.ceil(dom.t0 / step2) * step2;t <= dom.t1; t += step2) {
    el("line", { x1: x(t), x2: x(t), y1: m.t, y2: H - m.b }, axis);
    el("text", { x: x(t), y: H - 10, "text-anchor": "middle" }, axis).textContent = tickFmt.format(t);
  }
  const pts = (p) => p.pts.map(([t, v]) => `${x(t).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const base = el("g", { class: "base", "clip-path": "url(#area)" }, svg);
  for (const e of md.edges)
    el("polyline", { points: pts(e), class: "ride", stroke: trainColor(e.trip) }, base);
  for (const pi of md.waits)
    el("polyline", { points: pts(item(D.pieces, pi)), class: "wait" }, base);
  for (const pi of md.walks)
    el("polyline", { points: pts(item(D.pieces, pi)), class: "walk" }, base);
  const overlay = el("g", { class: "overlay", "clip-path": "url(#area)" }, svg);
  const hits = el("g", { "clip-path": "url(#area)" }, svg);
  md.targets.forEach((t, i) => el("polyline", { points: pts(t), class: "hit", "data-t": i }, hits));
  const timeAt = (e) => dom.t0 + (e.clientX - svg.getBoundingClientRect().left - m.l) / (W - m.l - m.r) * (dom.t1 - dom.t0);
  const byDistance = (e, key) => {
    const r = svg.getBoundingClientRect(), px = e.clientX - r.left, py = e.clientY - r.top;
    const found = [];
    for (const h of document.elementsFromPoint(e.clientX, e.clientY)) {
      if (!(h instanceof SVGPolylineElement))
        continue;
      const i = h.dataset[key];
      if (i !== undefined)
        found.push([distanceToLine(px, py, [...h.points]), Number(i)]);
    }
    return found.sort((a, b) => a[0] - b[0]).map(([, i]) => i);
  };
  const nearest = (e, key) => byDistance(e, key)[0] ?? null;
  const targetAt = (e) => {
    const time = timeAt(e);
    for (const i of byDistance(e, "t")) {
      const t = item(md.targets, i).at(time);
      if (live(t))
        return t;
    }
    return null;
  };
  hits.addEventListener("mousemove", (e) => {
    if (drag)
      return;
    hoverTarget = targetAt(e);
    refresh();
    moveTip(e);
  });
  hits.addEventListener("mouseout", () => {
    hoverTarget = null;
    refresh();
  });
  hits.addEventListener("click", (e) => {
    e.stopPropagation();
    if (dragged)
      return;
    const t = targetAt(e);
    if (t)
      choose(t);
  });
  const hoverLayer = el("g", { "clip-path": "url(#area)" }, svg);
  const fbLayer = el("g", { class: "fallback", "clip-path": "url(#area)" }, svg);
  const fbTargetAt = (e) => {
    const i = nearest(e, "f");
    return i === null ? null : view?.fbTargets[i]?.at(timeAt(e)) ?? null;
  };
  fbLayer.addEventListener("mousemove", (e) => {
    if (drag)
      return;
    hoverTarget = fbTargetAt(e);
    refresh();
    moveTip(e);
  });
  fbLayer.addEventListener("mouseout", () => {
    hoverTarget = null;
    refresh();
  });
  fbLayer.addEventListener("click", (e) => {
    e.stopPropagation();
  });
  const labels = el("g", { class: "change-labels", "clip-path": "url(#area)" }, svg);
  svg.addEventListener("click", () => {
    if (dragged)
      return;
    clearPicks();
    refresh();
  });
  view = { svg, x, y, pts, overlay, hoverLayer, fbLayer, labels, overlayKey: null, fbKey: undefined, fbTargets: [] };
  refresh();
}
var frame = null;
function scheduleRender() {
  frame ??= requestAnimationFrame(() => {
    frame = null;
    render();
  });
}
function mouseInPlot(e, { W, H, m }) {
  const r = plotEl.getBoundingClientRect();
  const px = e.clientX - r.left, py = e.clientY - r.top;
  return { px, py, fx: (px - m.l) / (W - m.l - m.r), fy: (py - m.t) / (H - m.t - m.b) };
}
plotEl.addEventListener("wheel", (e) => {
  e.preventDefault();
  if (!geom)
    return;
  const { fx, fy } = mouseInPlot(e, geom);
  dom = zoom(dom, full, { fx, fy }, Math.exp(e.deltaY * 0.0015), { time: !e.altKey, height: !e.shiftKey });
  scheduleRender();
}, { passive: false });
var drag = null;
var dragged = false;
var fingers = new Map;
var pinching = null;
var inPlot = (e) => {
  const r = plotEl.getBoundingClientRect();
  return { x: e.clientX - r.left, y: e.clientY - r.top };
};
var two = () => {
  const [a, b] = fingers.values();
  return a !== undefined && b !== undefined ? [a, b] : null;
};
plotEl.addEventListener("pointerdown", (e) => {
  if (e.pointerType === "mouse" && e.button !== 0)
    return;
  fingers.set(e.pointerId, inPlot(e));
  const pair2 = two();
  if (pair2) {
    pinching = { from: pair2, dom: { ...dom } };
    drag = null;
    dragged = true;
    tip.style.display = "none";
  } else {
    drag = { x: e.clientX, y: e.clientY, dom: { ...dom } };
    dragged = false;
  }
});
window.addEventListener("pointermove", (e) => {
  if (!fingers.has(e.pointerId) || !geom)
    return;
  fingers.set(e.pointerId, inPlot(e));
  const pair2 = two();
  if (pinching && pair2) {
    dom = pinch(pinching.dom, full, pinching.from, pair2, geom);
    scheduleRender();
    return;
  }
  if (!drag)
    return;
  const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
  if (!dragged && Math.hypot(dx, dy) < 4)
    return;
  dragged = true;
  tip.style.display = "none";
  dom = pan(drag.dom, full, dx, dy, geom);
  scheduleRender();
});
var lift = (e) => {
  if (!fingers.delete(e.pointerId))
    return;
  pinching = null;
  drag = null;
  if (fingers.size === 0 && dragged)
    setTimeout(() => {
      dragged = false;
    }, 0);
};
window.addEventListener("pointerup", lift);
window.addEventListener("pointercancel", lift);
plotEl.addEventListener("dblclick", () => {
  dom = { ...full };
  scheduleRender();
});
byId("reset", HTMLButtonElement).addEventListener("click", () => {
  dom = { ...full };
  scheduleRender();
});
function choose(target) {
  picks = toggle(picks, {
    id: target.id,
    info: target.info,
    pts: target.pts,
    chains: target.chains,
    reach: target.reach ?? null
  });
  applyPicks();
  refresh();
}
function applyPicks() {
  candidates = narrow(picks, (i) => !hidden.has(item(D.chains, i).group));
  pinned = candidates === null ? null : pinAfter(candidates.chains, pinned);
}
function clearPicks() {
  picks = [];
  candidates = null;
  pinned = null;
  preview = null;
}
function live(t) {
  const segments = candidates?.reach?.segments;
  if (segments === undefined || t.spot === undefined || picks.some((p) => p.id === t.id))
    return true;
  return covers(segments, D.pieces, t.spot);
}
var hoverPick = null;
function previewOf(h) {
  if (h === null || !("id" in h) || typeof h.id !== "string" || picks.length === 0)
    return null;
  const target = { id: h.id, info: h.info, pts: h.pts, chains: h.chains, reach: h.reach ?? null };
  const key = JSON.stringify([target.id, target.reach?.key, picks.map((p) => p.id), [...hidden]]);
  if (hoverPick?.key !== key)
    hoverPick = { key, result: narrow(toggle(picks, target), (i) => !hidden.has(item(D.chains, i).group)) };
  return hoverPick.result;
}
function refresh() {
  if (!view)
    return;
  const { svg, x, y, pts, overlay, hoverLayer, fbLayer, labels } = view;
  const strong = [pinned, preview].filter((i) => i !== null);
  const soft = new Set, faded = new Set;
  const pv = previewOf(hoverTarget);
  const reach = pv ? pv.reach : hoverTarget?.reach ?? candidates?.reach ?? null;
  const reachFaded = !hoverTarget?.reach && strong.length > 0;
  if (pv)
    pv.chains.forEach((i) => soft.add(i));
  else if (hoverTarget && !hoverTarget.reach)
    hoverTarget.chains.forEach((i) => soft.add(i));
  else if (hoverTarget?.reach?.with)
    hoverTarget.reach.with.forEach((i) => soft.add(i));
  if (softGroup !== null)
    D.chains.forEach((c, i) => {
      if (c.group === softGroup && !hidden.has(c.group))
        soft.add(i);
    });
  if (candidates)
    candidates.chains.forEach((i) => {
      if (!soft.has(i))
        (strong.length || pv ? faded : soft).add(i);
    });
  svg.classList.toggle("focus", strong.length > 0 || soft.size > 0 || faded.size > 0 || reach !== null);
  const key = JSON.stringify([
    strong,
    [...soft].sort((a, b) => a - b),
    [...faded].sort((a, b) => a - b),
    reach?.key,
    reachFaded
  ]);
  if (key !== view.overlayKey) {
    view.overlayKey = key;
    overlay.replaceChildren();
    const draw = (pi, cls, from = -Infinity, to = Infinity) => {
      const p = item(D.pieces, pi);
      const s = clipLine(p.pts, from, to);
      if (s.length < 2 && p.pts.length >= 2)
        return;
      const line = el("polyline", { points: pts({ pts: s }), class: `${p.kind}${p.warn ? " warn" : ""} ${cls}` }, overlay);
      if (p.kind === "ride" && p.trip !== null)
        line.setAttribute("stroke", trainColor(p.trip));
    };
    const mine = new Set(strong.flatMap((i) => item(D.chains, i).pieces));
    const shown = new Set(mine);
    const pieces = (chains) => new Set([...chains].flatMap((i) => item(D.chains, i).pieces).filter((pi) => !shown.has(pi)));
    const bright = pieces(soft);
    bright.forEach((pi) => shown.add(pi));
    if (reach && reachFaded) {
      for (const [pi, ...cut] of reach.segments)
        if (!shown.has(pi))
          draw(pi, "soft faded", ...cut);
    }
    for (const pi of pieces(faded))
      draw(pi, "soft faded");
    for (const pi of bright)
      draw(pi, "soft");
    if (reach && !reachFaded) {
      for (const [pi, ...cut] of reach.segments)
        if (!shown.has(pi))
          draw(pi, "soft", ...cut);
    }
    for (const pi of mine)
      draw(pi, "strong");
  }
  hoverLayer.replaceChildren();
  for (const p of picks)
    el("polyline", { points: pts(p), class: "picked-piece" }, hoverLayer);
  if (hoverTarget)
    el("polyline", { points: pts(hoverTarget), class: "hover-piece" }, hoverLayer);
  const fbKey = (focus2) => focus2 === pinned ? focus2 : null;
  const focus = preview ?? pinned;
  const fbDetail = ((k) => k === null ? undefined : detailOf(k))(fbKey(focus));
  const fk = fbDetail === undefined ? null : fbKey(focus);
  if (fk !== view.fbKey) {
    view.fbKey = fk;
    fbLayer.replaceChildren();
    const fbTargets = [];
    view.fbTargets = fbTargets;
    if (fbDetail !== undefined) {
      const byPiece = new Map, waits = new Map;
      for (const f of fbDetail.fallbacks) {
        const g = el("g", {}, fbLayer);
        for (const p of f.pieces) {
          const line = el("polyline", { points: pts(p), class: `${p.kind}${p.warn ? " warn" : ""}` }, g);
          if (p.kind === "ride" && p.trip !== null)
            line.setAttribute("stroke", trainColor(p.trip));
          const first = p.pts[0], last = p.pts[p.pts.length - 1];
          if (!first || !last)
            continue;
          if (p.kind === "wait") {
            const [from, wy, s] = first, [to] = last;
            entry(waits, s, () => ({ y: wy, spans: [] })).spans.push({ from, to, f });
          } else {
            entry(byPiece, p, () => new Set).add(f);
          }
        }
      }
      const hit = (points, at) => {
        fbTargets.push({ at });
        el("polyline", { points: pts({ pts: points }), class: "hit", "data-f": fbTargets.length - 1 }, fbLayer);
      };
      for (const [p, fs] of byPiece) {
        const t = {
          info: p.info,
          ...p.trip === null ? {} : { trip: p.trip },
          chains: [],
          fallbacks: [...fs],
          pts: p.pts
        };
        hit(p.pts, () => t);
      }
      for (const [s, { y: wy, spans }] of waits) {
        for (const [from, to] of runsOf(spans))
          hit([
            [from, wy, s],
            [to, wy, s]
          ], (time) => {
            const { t, here, pts: stayPts } = stayAt(spans, from, to, time, wy, s);
            return {
              info: `waiting at ${item(D.names, s)} · ${timeFmt.format(t)}`,
              chains: [],
              fallbacks: [...new Set(here.map((w) => w.f))],
              pts: stayPts
            };
          });
      }
    }
  }
  labels.replaceChildren();
  if (focus !== null) {
    for (const p of item(D.chains, focus).pieces.map((i) => item(D.pieces, i)).filter((p2) => p2.kind === "wait")) {
      const first = p.pts[0];
      if (!first)
        continue;
      const [t, v] = first;
      el("text", { x: x(t) + 4, y: y(v) - 5 }, labels).textContent = p.info;
    }
  }
  if (hoverTarget) {
    const { info, trip } = hoverTarget;
    const swatch = trip === undefined ? "" : `<span style="color:${trainColor(trip)}">■</span> `;
    const who = whoAt(hoverTarget, pv);
    tip.innerHTML = `<b>${swatch}${esc(info)}</b>${who}`;
    tip.style.display = "block";
  } else {
    tip.style.display = "none";
  }
  renderPanel();
}
var muted = (html) => `<br><span style="color:var(--muted)">${html}</span>`;
function whoAt(h, pv) {
  if (h.fallbacks) {
    const at = [...new Set(h.fallbacks.map((f) => f.at))];
    return `<br><span style="color:var(--accent)">If the change at ${esc(at.join(" / "))} is missed:</span>` + h.fallbacks.map((f) => muted(`• ${esc(f.line)} · arrive ${esc(f.arrive)}`)).join("");
  }
  const chains = pv ? pv.chains : h.chains;
  const groups = new Map;
  for (const i of chains) {
    const g = item(D.chains, i).group;
    groups.set(g, (groups.get(g) ?? 0) + 1);
  }
  const k = String(chains.length);
  let note;
  const picked = "id" in h && picks.some((p) => p.id === h.id);
  if (picked)
    note = pv ? `picked · click to remove it: ${k} connections pass the other picks` : "picked · click to remove it";
  else if (pv === null)
    note = chains.length ? `${k} of the listed connections in ${String(groups.size)} group${groups.size > 1 ? "s" : ""} · click to list them` : "none of the listed connections · on others within the limits";
  else
    note = chains.length ? `${k} of the ${String(candidates?.chains.length ?? 0)} connections passing your picks · click to narrow to them` : "none together with your picks";
  const only = chains.length === 1 ? chains[0] : undefined;
  if (only !== undefined) {
    want([only]);
    const one = `<br><b>${esc(item(D.chains, only).header)}</b><span style="color:var(--muted)">${esc(routeOf(only))}</span>`;
    return pv === null && !picked ? one : muted(note) + one;
  }
  const list = [...groups].sort((a, b) => b[1] - a[1]).map(([g, n]) => `• ${esc(item(D.groups, g).label)} (${String(n)})`).join("<br>");
  return muted(note) + (list ? `<br>${list}` : "");
}
function moveTip(e) {
  const r = plotEl.getBoundingClientRect();
  tip.style.left = `${String(Math.min(e.clientX - r.left + 14, r.width - tip.offsetWidth - 8))}px`;
  tip.style.top = `${String(e.clientY - r.top + 14)}px`;
}
var panelKey = null;
function renderPanel() {
  const key = JSON.stringify([pinned, picks.map((p) => p.id), candidates?.chains.length ?? null]);
  if (key === panelKey)
    return;
  panelKey = key;
  picksEl.replaceChildren();
  for (const p of picks) {
    const row = document.createElement("div");
    row.className = "pick";
    row.title = "Click to remove";
    row.innerHTML = `<span>${esc(p.info)}</span><span class="x">×</span>`;
    row.addEventListener("click", () => {
      choose(p);
    });
    picksEl.appendChild(row);
  }
  if (picks.length) {
    const hint = document.createElement("div");
    hint.className = "pick-hint";
    hint.textContent = "Click more lines or stations to narrow down; Esc clears.";
    picksEl.appendChild(hint);
  }
  if (pinned !== null) {
    panelTitle.textContent = "Selected connection";
    const c = item(D.chains, pinned);
    const d = detailOf(pinned);
    let html = candidates && candidates.chains.length > 1 ? `<span class="back" id="back">← all ${String(candidates.chains.length)} connections</span>` : "";
    const route = routeOf(pinned);
    html += `<b>${esc(c.header)}</b>
${esc(route).replaceAll(" ‖", `
‖`).replaceAll("‖ ", `‖
`)}`;
    let at = null;
    for (const f of d?.fallbacks ?? []) {
      if (f.at !== at) {
        html += `

<span class="fb">If the change at ${esc(f.at)} is missed:</span>`;
        at = f.at;
      }
      html += `
<span class="fb">• ${esc(f.line)} · arrive ${esc(f.arrive)}</span>`;
    }
    details.innerHTML = html;
    document.getElementById("back")?.addEventListener("click", () => {
      pinned = null;
      refresh();
    });
  } else if (candidates) {
    panelTitle.textContent = picks.length > 1 ? `${String(candidates.chains.length)} connections pass all ${String(picks.length)}` : `${String(candidates.chains.length)} listed connections use it`;
    details.replaceChildren();
    const shown = candidates.chains.slice(0, MAX_LISTED);
    want(shown);
    for (const i of shown) {
      const c = item(D.chains, i);
      const row = document.createElement("div");
      row.className = "candidate";
      const route = routeOf(i);
      row.innerHTML = `<b>${esc(c.header)}</b><div class="route">${esc(route)}</div>`;
      row.addEventListener("mouseenter", () => {
        preview = i;
        refresh();
      });
      row.addEventListener("mouseleave", () => {
        preview = null;
        refresh();
      });
      row.addEventListener("click", () => {
        preview = null;
        pinned = i;
        refresh();
      });
      details.appendChild(row);
    }
    const more = candidates.chains.length - shown.length;
    if (more > 0) {
      const note = document.createElement("div");
      note.className = "pick-hint";
      note.textContent = `… and ${String(more)} more: click lines or stations to narrow down.`;
      details.appendChild(note);
    }
  } else {
    panelTitle.textContent = "Selection";
    details.textContent = HINT;
  }
}
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    clearPicks();
    refresh();
    return;
  }
  if (!candidates || e.key !== "ArrowDown" && e.key !== "ArrowUp")
    return;
  e.preventDefault();
  pinned = step(candidates.chains, pinned, e.key === "ArrowDown");
  refresh();
});
var groupsEl = byId("groups", HTMLDivElement);
D.groups.forEach((g, i) => {
  const row = document.createElement("div");
  row.className = "group";
  row.innerHTML = `<span>${esc(g.label)}</span><span class="count">${String(g.count)}</span>`;
  row.addEventListener("mouseenter", () => {
    softGroup = hidden.has(i) ? null : i;
    refresh();
  });
  row.addEventListener("mouseleave", () => {
    softGroup = null;
    refresh();
  });
  row.addEventListener("click", () => {
    if (hidden.has(i))
      hidden.delete(i);
    else
      hidden.add(i);
    row.classList.toggle("off", hidden.has(i));
    softGroup = null;
    if (pinned !== null && item(D.chains, pinned).group === i)
      pinned = null;
    applyPicks();
    panelKey = null;
    buildModel();
    render();
  });
  groupsEl.appendChild(row);
});
buildModel();
new ResizeObserver(render).observe(plotEl);
