// decode.ts
class DecodeError extends Error {
}
function fail(path, expected) {
  throw new DecodeError(`${path}: expected ${expected}`);
}
function toJson(value, path) {
  if (value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return value;
  }
  if (Array.isArray(value))
    return value.map((x, i) => toJson(x, `${path}[${String(i)}]`));
  if (typeof value === "object") {
    const out = {};
    for (const [key, x] of Object.entries(value))
      out[key] = toJson(x, `${path}.${key}`);
    return out;
  }
  return fail(path, "JSON");
}
function parseJson(text) {
  const value = JSON.parse(text);
  return toJson(value, "$");
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
  const v = o[name];
  return v === undefined ? fail(`${path}.${name}`, "a value") : decode(v, `${path}.${name}`);
}
var record = (decode) => (v, path) => {
  const o = object(v, path);
  const out = {};
  for (const [key, x] of Object.entries(o))
    out[key] = decode(x, `${path}.${key}`);
  return out;
};

// dom.ts
function byId(id, type) {
  const element = document.getElementById(id);
  if (!(element instanceof type))
    throw new Error(`#${id} missing`);
  return element;
}

// plot.ts
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
        route: field(oc, "route", string, p),
        pieces: field(oc, "pieces", array(number), p),
        fallbacks: field(oc, "fallbacks", array(pair(number, string)), p)
      };
    }), path)
  };
};
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
function load(data) {
  const { pieces: rawPieces, fallbacks: rawFallbacks, chains: rawChains, ...rest } = data;
  const pieces = rawPieces.map(({ kind, info, warn, trip, pts }) => ({
    kind,
    info,
    warn,
    trip,
    raw: pts.map(([m, s]) => [m * 60000, s]),
    pts: []
  }));
  const fallbacks = rawFallbacks.map((f) => ({ at: f.at, line: f.line, pieces: f.pieces.map((i) => item(pieces, i)) }));
  const chains = rawChains.map(({ group, header, route, pieces: ps, fallbacks: fs }) => ({
    group,
    header,
    route,
    pieces: ps,
    fallbacks: fs.map(([i, arrive]) => ({ ...item(fallbacks, i), arrive }))
  }));
  return { ...rest, pieces, chains };
}
var D = load(decodeData(parseJson(byId("data", HTMLScriptElement).text), "data"));
var NS = "http://www.w3.org/2000/svg";
var plotEl = byId("plot", HTMLDivElement);
var tip = byId("tip", HTMLDivElement);
var details = byId("details", HTMLDivElement);
var panelTitle = byId("panel-title", HTMLHeadingElement);
var picksEl = byId("picks", HTMLDivElement);
var timeFmt = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
  timeZone: D.tz
});
var trainColor = (t) => `hsl(${String(t * 137.508 % 360)}, 70%, 45%)`;
var HINT = "Hover a line or station name to see what is reachable through it. Click it to pick it: the connections " + "passing it are listed (one alone is selected). Click more to narrow down, a pick again to remove it.";
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
  legend.innerHTML = `x: time (${D.tz}). y: ${metric.description} (${metric.unit}), goal at the bottom.<br>` + `Coloured: riding, one colour per train · thin grey: waiting (red: below minimum transfer, or missed by live data ✗) · dotted: walking.<br>` + `Parallel lines: same route later. Steeper: faster. Dashed: fallbacks of the selection (orange dots: waiting).<br>` + `<kbd>↑</kbd> <kbd>↓</kbd> step through listed connections, <kbd>Esc</kbd> clears the selection.<br>` + `Wheel: zoom (<kbd>Shift</kbd> time only, <kbd>Alt</kbd> height only) · drag: pan · double-click: reset.`;
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
  for (const p of D.pieces)
    p.pts = p.raw.flatMap(([t, s]) => {
      const h = heightOf(s);
      return h === null ? [] : [[t, h, s]];
    });
  rows = D.stations.flatMap((s) => {
    const h = heightOf(s);
    return h === null ? [] : [{ i: s, name: item(D.names, s), y: h }];
  }).sort((a, b) => b.y - a.y || a.name.localeCompare(b.name));
  Object.assign(full, { t0: Infinity, t1: -Infinity, y0: 0, y1: 0 });
  for (const c of D.chains)
    for (const i of c.pieces)
      for (const [t, v] of item(D.pieces, i).pts) {
        full.t0 = Math.min(full.t0, t);
        full.t1 = Math.max(full.t1, t);
        full.y1 = Math.max(full.y1, v);
      }
  full.t0 -= 10 * 60000;
  full.t1 += 10 * 60000;
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
function runsOf(spans) {
  const runs = [];
  for (const w of [...spans].sort((a, b) => a.from - b.from)) {
    const last = runs[runs.length - 1];
    if (last && w.from <= last[1])
      last[1] = Math.max(last[1], w.to);
    else
      runs.push([w.from, w.to]);
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
function reachSegments(items) {
  const from = new Map, upTo = new Map;
  const before = (a, b) => a[0] < b[0] || a[0] === b[0] && a[1] < b[1];
  for (const { spans, t, back, fwd } of items)
    for (const w of spans) {
      if (fwd && w.to >= t) {
        const p = w.k === undefined ? [w.j, 0, t] : [w.j, w.k - 1, t], q = from.get(w.chain);
        if (!q || before(p, q))
          from.set(w.chain, p);
      }
      if (back && w.from <= t) {
        const n = item(D.pieces, item(item(D.chains, w.chain).pieces, w.j)).pts.length - 1;
        const p = w.k === undefined ? [w.j, n, t] : [w.j, w.k, t], q = upTo.get(w.chain);
        if (!q || before(q, p))
          upTo.set(w.chain, p);
      }
    }
  const raw = new Map;
  const add = (pi, k0, k1, c0 = null, c1 = null) => {
    const s = item(D.pieces, pi).pts;
    if (!s.length)
      return;
    let a = item(s, k0)[0], b = item(s, k1)[0];
    if (c0 !== null && k0 < k1 && item(s, k0)[2] === item(s, k0 + 1)[2])
      a = Math.min(Math.max(a, c0), item(s, k0 + 1)[0]);
    if (c1 !== null && k1 > k0 && item(s, k1)[2] === item(s, k1 - 1)[2])
      b = Math.max(Math.min(b, c1), item(s, k1 - 1)[0]);
    entry(raw, pi, () => []).push([a, b]);
  };
  const last = (pi) => item(D.pieces, pi).pts.length - 1;
  for (const [chain, [j, k0, t]] of from) {
    const ps = item(D.chains, chain).pieces;
    add(item(ps, j), k0, last(item(ps, j)), t, null);
    for (let q = j + 1;q < ps.length; q++)
      add(item(ps, q), 0, last(item(ps, q)));
  }
  for (const [chain, [j, k1, t]] of upTo) {
    const ps = item(D.chains, chain).pieces;
    for (let q = 0;q < j; q++)
      add(item(ps, q), 0, last(item(ps, q)));
    add(item(ps, j), 0, k1, null, t);
  }
  const out = [];
  for (const [pi, list] of raw)
    for (const [a, b] of mergeIntervals(list))
      out.push([pi, a, b]);
  return out;
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
function buildModel() {
  const edges = new Map, waits = new Map, walks = new Map, warns = new Set;
  const stays = new Map;
  const stay = (s, y, span) => {
    entry(stays, s, () => ({ y, spans: [] })).spans.push(span);
  };
  D.chains.forEach((c, i) => {
    if (hidden.has(c.group))
      return;
    c.pieces.forEach((pi, j) => {
      const p = item(D.pieces, pi);
      if (p.kind === "ride" && p.trip !== null) {
        const trip = p.trip;
        for (let k = 1;k < p.pts.length; k++) {
          const a = item(p.pts, k - 1), b = item(p.pts, k);
          const key = `${String(trip)}:${String(a[0])}:${String(a[2])}:${String(b[0])}:${String(b[2])}`;
          entry(edges, key, () => {
            const info = `${item(D.trips, trip)} · ${timeFmt.format(a[0])} ${item(D.names, a[2])} → ${timeFmt.format(b[0])} ${item(D.names, b[2])}`;
            return { trip, pts: [a, b], info, chains: new Set };
          }).chains.add(i);
          if (a[2] === b[2])
            stay(a[2], a[1], { from: a[0], to: b[0], chain: i, trip, j, k });
        }
      } else if (p.kind === "wait") {
        const first = p.pts[0], last = p.pts[p.pts.length - 1];
        if (!first || !last)
          return;
        const s = first[2], span = { from: first[0], to: last[0], chain: i, j };
        entry(waits, s, () => ({ y: first[1], spans: [], runs: [] })).spans.push(span);
        stay(s, first[1], span);
        if (p.warn)
          warns.add(pi);
      } else if (p.kind === "walk") {
        entry(walks, pi, () => new Set).add(i);
      }
    });
  });
  for (const w of waits.values())
    w.runs = runsOf(w.spans);
  const targets = [];
  const conns = [...edges.values()].map((e) => ({
    trip: e.trip,
    from: e.pts[0][2],
    dep: e.pts[0][0],
    to: e.pts[1][2],
    arr: e.pts[1][0]
  }));
  const byDep = [...conns].sort((a, b) => a.dep - b.dep), byArr = [...conns].sort((a, b) => b.arr - a.arr);
  const foot = new Map, footBack = new Map;
  for (const pi of walks.keys()) {
    const p = item(D.pieces, pi);
    const a = p.pts[0], b = p.pts[p.pts.length - 1];
    if (p.pts.length < 2 || !a || !b)
      continue;
    entry(foot, a[2], () => new Map).set(b[2], b[0] - a[0]);
    entry(footBack, b[2], () => new Map).set(a[2], b[0] - a[0]);
  }
  const closure = (src) => {
    const ea = new Map, ld = new Map;
    const early = (x, t) => {
      if (atMost(ea.get(x), t))
        return;
      ea.set(x, t);
      for (const [y, d] of foot.get(x) ?? [])
        if (!atMost(ea.get(y), t + d))
          ea.set(y, t + d);
    };
    const late = (x, t) => {
      if (atLeast(ld.get(x), t))
        return;
      ld.set(x, t);
      for (const [y, d] of footBack.get(x) ?? [])
        if (!atLeast(ld.get(y), t - d))
          ld.set(y, t - d);
    };
    for (const [s, t] of src.at) {
      early(s, t);
      late(s, t);
    }
    for (const [s, t] of src.back ?? [])
      late(s, t);
    for (const [s, t] of src.fwd ?? [])
      early(s, t);
    const boarded = new Map(src.ride ? [[src.ride.trip, src.ride.from]] : []);
    for (const c of byDep) {
      const since = boarded.get(c.trip);
      if (since !== undefined && c.dep >= since || atMost(ea.get(c.from), c.dep)) {
        if (since === undefined)
          boarded.set(c.trip, c.dep);
        early(c.to, c.arr);
      }
    }
    const alighted = new Map(src.ride ? [[src.ride.trip, src.ride.until]] : []);
    for (const c of byArr) {
      const until = alighted.get(c.trip);
      if (until !== undefined && c.arr <= until || atLeast(ld.get(c.to), c.arr)) {
        if (until === undefined)
          alighted.set(c.trip, c.arr);
        late(c.from, c.dep);
      }
    }
    const items = [];
    for (const [x, t] of ea) {
      const st = stays.get(x);
      if (st)
        items.push({ spans: st.spans, t, back: false, fwd: true });
    }
    for (const [x, t] of ld) {
      const st = stays.get(x);
      if (st)
        items.push({ spans: st.spans, t, back: true, fwd: false });
    }
    return reachSegments(items);
  };
  for (const e of edges.values()) {
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
          const waiting = new Set(here.filter((w) => w.trip === undefined).map((w) => w.chain)).size;
          const parts = [
            waiting ? `${String(waiting)} waiting` : null,
            ...trains.map((tr) => `standing: ${item(D.trips, tr)}`)
          ].filter((x) => x !== null);
          const key = `${String(s)}:${String(spans.filter((w) => w.from <= t).length)}:${String(spans.filter((w) => w.to >= t).length)}`;
          const r = reach?.key === key ? reach : { key, segments: closure({ at: [[s, t]] }) };
          reach = r;
          const trip = trains.length === 1 && !waiting ? trains[0] : undefined;
          return {
            id: `stay:${String(s)}:${String(from)}`,
            info: `at ${item(D.names, s)} · ${timeFmt.format(t)} · ${parts.join(" · ")}`,
            ...trip === undefined ? {} : { trip },
            chains: [...new Set(here.map((w) => w.chain))],
            reach: r,
            pts
          };
        }
      });
  }
  for (const [pi, chains] of walks) {
    const p = item(D.pieces, pi);
    const t = { id: `walk:${String(pi)}`, info: p.info, chains: [...chains], pts: p.pts };
    targets.push({ pts: t.pts, at: () => t });
  }
  const touching = new Map;
  D.chains.forEach((c, i) => {
    if (hidden.has(c.group))
      return;
    for (const pi of c.pieces)
      for (const [, , s] of item(D.pieces, pi).pts)
        entry(touching, s, () => new Set).add(i);
  });
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
    edges: [...edges.values()],
    waits: [...waits.values()],
    walks: [...walks.keys()],
    warns: [...warns],
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
  const m = { l: 240, r: 16, t: 14, b: 28 };
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
  const at = st.map((s) => s.py);
  for (let k = 1;k < at.length; k++)
    at[k] = Math.max(item(at, k), item(at, k - 1) + gap);
  const lastAt = at[at.length - 1];
  if (lastAt !== undefined && lastAt > H - m.b) {
    at[at.length - 1] = H - m.b;
    for (let k = at.length - 2;k >= 0; k--)
      at[k] = Math.min(item(at, k), item(at, k + 1) - gap);
  }
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
    t.textContent = s.name.length > 32 ? s.name.slice(0, 31) + "…" : s.name;
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
  const steps = [5, 10, 15, 30, 60, 120, 180, 360].map((mins) => mins * 60000);
  const step = steps.find((s) => (dom.t1 - dom.t0) / s <= 12) ?? item(steps, steps.length - 1);
  for (let t = Math.ceil(dom.t0 / step) * step;t <= dom.t1; t += step) {
    el("line", { x1: x(t), x2: x(t), y1: m.t, y2: H - m.b }, axis);
    el("text", { x: x(t), y: H - 10, "text-anchor": "middle" }, axis).textContent = timeFmt.format(t);
  }
  const pts = (p) => p.pts.map(([t, v]) => `${x(t).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const base = el("g", { class: "base", "clip-path": "url(#area)" }, svg);
  for (const e of md.edges)
    el("polyline", { points: pts(e), class: "ride", stroke: trainColor(e.trip) }, base);
  for (const w of md.waits) {
    for (const [from, to] of w.runs)
      el("polyline", {
        points: pts({
          pts: [
            [from, w.y],
            [to, w.y]
          ]
        }),
        class: "wait"
      }, base);
  }
  for (const pi of md.warns)
    el("polyline", { points: pts(item(D.pieces, pi)), class: "wait warn" }, base);
  for (const pi of md.walks)
    el("polyline", { points: pts(item(D.pieces, pi)), class: "walk" }, base);
  const overlay = el("g", { class: "overlay", "clip-path": "url(#area)" }, svg);
  const hits = el("g", { "clip-path": "url(#area)" }, svg);
  md.targets.forEach((t, i) => el("polyline", { points: pts(t), class: "hit", "data-t": i }, hits));
  const timeAt = (e) => dom.t0 + (e.clientX - svg.getBoundingClientRect().left - m.l) / (W - m.l - m.r) * (dom.t1 - dom.t0);
  const nearest = (e, key) => {
    const r = svg.getBoundingClientRect(), px = e.clientX - r.left, py = e.clientY - r.top;
    let best = null, bestD = Infinity;
    for (const h of document.elementsFromPoint(e.clientX, e.clientY)) {
      if (!(h instanceof SVGPolylineElement))
        continue;
      const i = h.dataset[key];
      if (i === undefined)
        continue;
      const p = [...h.points];
      for (let k = 0;k < p.length; k++) {
        const a = item(p, k), b = item(p, Math.min(k + 1, p.length - 1));
        const dx = b.x - a.x, dy = b.y - a.y, len = dx * dx + dy * dy;
        const u = len ? Math.max(0, Math.min(1, ((px - a.x) * dx + (py - a.y) * dy) / len)) : 0;
        const d = Math.hypot(px - a.x - u * dx, py - a.y - u * dy);
        if (d < bestD) {
          bestD = d;
          best = Number(i);
        }
      }
    }
    return best;
  };
  const targetAt = (e) => {
    const i = nearest(e, "t");
    return i === null ? null : item(md.targets, i).at(timeAt(e));
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
function clampDomain() {
  const minT = 10 * 60000, minY = (full.y1 - full.y0) / 200;
  if (dom.t1 - dom.t0 < minT) {
    const c = (dom.t0 + dom.t1) / 2;
    dom.t0 = c - minT / 2;
    dom.t1 = c + minT / 2;
  }
  if (dom.y1 - dom.y0 < minY) {
    const c = (dom.y0 + dom.y1) / 2;
    dom.y0 = c - minY / 2;
    dom.y1 = c + minY / 2;
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
  [dom.t0, dom.t1] = span(dom.t0, dom.t1, full.t0, full.t1);
  [dom.y0, dom.y1] = span(dom.y0, dom.y1, full.y0, full.y1);
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
  const k = Math.exp(e.deltaY * 0.0015);
  if (!e.altKey) {
    const at = dom.t0 + fx * (dom.t1 - dom.t0);
    dom.t0 = at - (at - dom.t0) * k;
    dom.t1 = at + (dom.t1 - at) * k;
  }
  if (!e.shiftKey) {
    const at = dom.y1 - fy * (dom.y1 - dom.y0);
    dom.y0 = at - (at - dom.y0) * k;
    dom.y1 = at + (dom.y1 - at) * k;
  }
  clampDomain();
  scheduleRender();
}, { passive: false });
var drag = null;
var dragged = false;
plotEl.addEventListener("mousedown", (e) => {
  if (e.button !== 0)
    return;
  drag = { x: e.clientX, y: e.clientY, dom: { ...dom } };
  dragged = false;
});
window.addEventListener("mousemove", (e) => {
  if (!drag || !geom)
    return;
  const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
  if (!dragged && Math.hypot(dx, dy) < 4)
    return;
  dragged = true;
  tip.style.display = "none";
  const { W, H, m } = geom;
  const dt = dx / (W - m.l - m.r) * (drag.dom.t1 - drag.dom.t0);
  const dv = dy / (H - m.t - m.b) * (drag.dom.y1 - drag.dom.y0);
  dom = { t0: drag.dom.t0 - dt, t1: drag.dom.t1 - dt, y0: drag.dom.y0 + dv, y1: drag.dom.y1 + dv };
  clampDomain();
  scheduleRender();
});
window.addEventListener("mouseup", () => {
  drag = null;
  if (dragged)
    setTimeout(() => {
      dragged = false;
    }, 0);
});
plotEl.addEventListener("dblclick", () => {
  dom = { ...full };
  scheduleRender();
});
byId("reset", HTMLButtonElement).addEventListener("click", () => {
  dom = { ...full };
  scheduleRender();
});
function choose(target) {
  const k = picks.findIndex((p) => p.id === target.id);
  if (k >= 0)
    picks.splice(k, 1);
  else
    picks.push({ id: target.id, info: target.info, chains: target.chains, reach: target.reach ?? null });
  applyPicks();
  refresh();
}
function applyPicks() {
  const [first, ...rest] = picks;
  if (!first) {
    candidates = null;
    pinned = null;
    return;
  }
  let chains = first.chains.filter((i) => !hidden.has(item(D.chains, i).group));
  for (const p of rest) {
    const s = new Set(p.chains);
    chains = chains.filter((i) => s.has(i));
  }
  const reaches = picks.map((p) => p.reach).filter((r) => r !== null);
  const reach = reaches.length ? {
    key: picks.map((p) => p.reach?.key ?? p.id).join("|"),
    segments: intersectReach(reaches.map((r) => r.segments))
  } : null;
  candidates = { info: picks.map((p) => p.info).join(" + "), chains, reach };
  pinned = chains.length === 1 ? item(chains, 0) : pinned !== null && chains.includes(pinned) ? pinned : null;
}
function clearPicks() {
  picks = [];
  candidates = null;
  pinned = null;
  preview = null;
}
function refresh() {
  if (!view)
    return;
  const { svg, x, y, pts, overlay, hoverLayer, fbLayer, labels } = view;
  const strong = [pinned, preview].filter((i) => i !== null);
  const soft = new Set, faded = new Set;
  const reach = hoverTarget?.reach ?? candidates?.reach ?? null;
  const reachFaded = !hoverTarget?.reach && strong.length > 0;
  if (hoverTarget && !hoverTarget.reach)
    hoverTarget.chains.forEach((i) => soft.add(i));
  if (hoverTarget?.reach?.with)
    hoverTarget.reach.with.forEach((i) => soft.add(i));
  if (softGroup !== null)
    D.chains.forEach((c, i) => {
      if (c.group === softGroup && !hidden.has(c.group))
        soft.add(i);
    });
  if (candidates)
    candidates.chains.forEach((i) => (strong.length ? faded : soft).add(i));
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
      let s = p.pts;
      if (from > -Infinity || to < Infinity) {
        const at = (a, b, t) => [t, a[1] + (b[1] - a[1]) * (t - a[0]) / (b[0] - a[0]), a[2]];
        const cut = [];
        const push = (q) => {
          const l = cut[cut.length - 1];
          if (l?.[0] !== q[0] || l[1] !== q[1])
            cut.push(q);
        };
        for (let k = 1;k < s.length; k++) {
          const a = item(s, k - 1), b = item(s, k);
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
        s = cut;
      }
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
  if (hoverTarget)
    el("polyline", { points: pts(hoverTarget), class: "hover-piece" }, hoverLayer);
  const fbKey = (focus2) => focus2 === pinned ? focus2 : null;
  const focus = preview ?? pinned;
  const fk = fbKey(focus);
  if (fk !== view.fbKey) {
    view.fbKey = fk;
    fbLayer.replaceChildren();
    const fbTargets = [];
    view.fbTargets = fbTargets;
    if (fk !== null) {
      const byPiece = new Map, waits = new Map;
      for (const f of item(D.chains, fk).fallbacks) {
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
    const { info, chains, trip } = hoverTarget;
    const swatch = trip === undefined ? "" : `<span style="color:${trainColor(trip)}">■</span> `;
    let who;
    const only = chains.length === 1 ? chains[0] : undefined;
    if (hoverTarget.fallbacks) {
      const at = [...new Set(hoverTarget.fallbacks.map((f) => f.at))];
      who = `<br><span style="color:var(--accent)">If the change at ${esc(at.join(" / "))} is missed:</span>` + hoverTarget.fallbacks.map((f) => `<br><span style="color:var(--muted)">• ${esc(f.line)} · arrive ${esc(f.arrive)}</span>`).join("");
    } else if (only !== undefined) {
      const c = item(D.chains, only);
      who = `<br><b>${esc(c.header)}</b><span style="color:var(--muted)">${esc(c.route)}</span>`;
    } else {
      const groups = new Map;
      for (const i of chains) {
        const g = item(D.chains, i).group;
        groups.set(g, (groups.get(g) ?? 0) + 1);
      }
      const list = [...groups].sort((a, b) => b[1] - a[1]).map(([g, n]) => `• ${esc(item(D.groups, g).label)} (${String(n)})`).join("<br>");
      who = `<br><span style="color:var(--muted)">${String(chains.length)} connections in ${String(groups.size)} group${groups.size > 1 ? "s" : ""} · click to list them</span><br>${list}`;
    }
    tip.innerHTML = `<b>${swatch}${esc(info)}</b>${who}`;
    tip.style.display = "block";
  } else {
    tip.style.display = "none";
  }
  renderPanel();
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
    let html = candidates && candidates.chains.length > 1 ? `<span class="back" id="back">← all ${String(candidates.chains.length)} connections</span>` : "";
    html += `<b>${esc(c.header)}</b>
${esc(c.route).replaceAll(" ‖", `
‖`).replaceAll("‖ ", `‖
`)}`;
    let at = null;
    for (const f of c.fallbacks) {
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
    panelTitle.textContent = picks.length > 1 ? `${String(candidates.chains.length)} connections pass all ${String(picks.length)}` : `${String(candidates.chains.length)} connections use it`;
    details.replaceChildren();
    for (const i of candidates.chains) {
      const c = item(D.chains, i);
      const row = document.createElement("div");
      row.className = "candidate";
      row.innerHTML = `<b>${esc(c.header)}</b><div class="route">${esc(c.route)}</div>`;
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
  const list = candidates.chains;
  const now = pinned === null ? -1 : list.indexOf(pinned);
  const next = now < 0 ? 0 : (now + (e.key === "ArrowDown" ? 1 : list.length - 1)) % list.length;
  pinned = list[next] ?? null;
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
