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

// dom.ts
function byId(id, type) {
  const element = document.getElementById(id);
  if (!(element instanceof type))
    throw new Error(`#${id} missing`);
  return element;
}

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

// sketch.ts
var time = nullable(number);
var latLon = pair(number, number);
function tuple(v, path, n) {
  const xs = array((x) => x)(v, path);
  if (xs.length !== n)
    throw new DecodeError(`${path}: expected ${String(n)} values`);
  return xs;
}
var at = (xs, i, decode, path) => decode(item(xs, i), `${path}[${String(i)}]`);
var place = (v, path) => {
  const o = object(v, path);
  return { name: field(o, "name", string, path), at: field(o, "at", latLon, path) };
};
var seen = (v, path) => {
  const xs = tuple(v, path, 3);
  return [at(xs, 0, string, path), at(xs, 1, time, path), at(xs, 2, time, path)];
};
var stop = (v, path) => {
  const xs = tuple(v, path, 4);
  return {
    at: [at(xs, 0, number, path), at(xs, 1, number, path)],
    arrival: at(xs, 2, time, path),
    departure: at(xs, 3, time, path)
  };
};
var call = (v, path) => {
  const xs = tuple(v, path, 3);
  return [at(xs, 0, number, path), at(xs, 1, time, path), at(xs, 2, time, path)];
};
var station = (v, path) => {
  const o = object(v, path);
  return {
    name: field(o, "name", string, path),
    at: field(o, "at", nullable(latLon), path),
    height: field(o, "height", nullable(number), path)
  };
};
var decodeItem = (v, path) => {
  const o = object(v, path);
  const kind = field(o, "kind", oneOf("ends", "asked", "train", "timetable", "asked_train"), path);
  switch (kind) {
    case "ends":
      return {
        kind,
        from: field(o, "from", place, path),
        to: field(o, "to", place, path),
        depart_from: field(o, "depart_from", number, path),
        depart_until: field(o, "depart_until", number, path)
      };
    case "asked":
      return {
        kind,
        place: field(o, "place", place, path),
        span: field(o, "span", nullable(pair(number, number)), path),
        seen: field(o, "seen", array(seen), path)
      };
    case "train":
      return { kind, id: field(o, "id", string, path), stops: field(o, "stops", array(stop), path) };
    case "timetable":
      return {
        kind,
        stations: field(o, "stations", array(station), path),
        trains: field(o, "trains", array(array(call)), path)
      };
    case "asked_train":
      return { kind, trip: field(o, "trip", number, path) };
  }
};
var emptyPicture = () => ({ ends: null, trains: new Map, timetable: null });
var when = (s) => s.arrival ?? s.departure ?? 0;
function addStop(stops, add) {
  if (add.arrival === null && add.departure === null)
    return;
  const same = stops.find((s) => s.at[0] === add.at[0] && s.at[1] === add.at[1]);
  if (same) {
    same.arrival ??= add.arrival;
    same.departure ??= add.departure;
    return;
  }
  const after = stops.findIndex((s) => when(s) > when(add));
  stops.splice(after < 0 ? stops.length : after, 0, { at: add.at, arrival: add.arrival, departure: add.departure });
}
function apply(picture, it) {
  const train = (id) => {
    let stops = picture.trains.get(id);
    if (stops === undefined)
      picture.trains.set(id, stops = []);
    return stops;
  };
  switch (it.kind) {
    case "ends":
      picture.ends = { from: it.from, to: it.to, depart_from: it.depart_from, depart_until: it.depart_until };
      return null;
    case "asked":
      if (picture.timetable === null) {
        for (const [id, arrival, departure] of it.seen)
          addStop(train(id), { at: it.place.at, arrival, departure });
      }
      return { asked: it.place, span: it.span };
    case "train":
      if (picture.timetable === null)
        for (const s of it.stops)
          addStop(train(it.id), s);
      return null;
    case "timetable":
      picture.timetable = { stations: it.stations, trains: it.trains };
      picture.trains.clear();
      return { timetable: true };
    case "asked_train":
      return { askedTrain: it.trip };
  }
}
var EARTH_KM = 6371;
var rad = (deg) => deg * Math.PI / 180;
function km(a, b) {
  const h = Math.sin(rad(b[0] - a[0]) / 2) ** 2 + Math.cos(rad(a[0])) * Math.cos(rad(b[0])) * Math.sin(rad(b[1] - a[1]) / 2) ** 2;
  return 2 * EARTH_KM * Math.asin(Math.sqrt(h));
}
function axis(at2, ends) {
  const total = km(ends.from.at, ends.to.at);
  if (total === 0)
    return 0;
  const a = km(at2, ends.from.at);
  const b = km(at2, ends.to.at);
  return total - (a * a + total * total - b * b) / (2 * total);
}
function spread(values, cut, keep) {
  if (values.length === 0)
    return keep;
  const sorted = values.toSorted((a, b) => a - b);
  const k = Math.floor(cut * (sorted.length - 1));
  return [Math.min(keep[0], item(sorted, k)), Math.max(keep[1], item(sorted, sorted.length - 1 - k))];
}
function corners(stops) {
  const points = [];
  for (const [height, arrival, departure] of stops) {
    if (height === null)
      continue;
    if (arrival !== null)
      points.push([arrival, height]);
    if (departure !== null && departure !== arrival)
      points.push([departure, height]);
  }
  return points.length < 2 ? [] : points;
}
function nearest(stations, at2, within) {
  let best = null;
  let bestKm = within;
  for (const [i, s] of stations.entries()) {
    if (s.at === null)
      continue;
    const d = km(s.at, at2);
    if (d < bestKm)
      [best, bestKm] = [i, d];
  }
  return best;
}

// status.ts
var decodeStatus = (v, path) => {
  const o = object(v, path);
  return {
    log: field(o, "log", array(string), path),
    now: field(o, "now", nullable(string), path),
    done: field(o, "done", boolean, path),
    error: field(o, "error", nullable(string), path),
    no_connection: field(o, "no_connection", nullable(string), path),
    back: field(o, "back", string, path),
    sketch: field(o, "sketch", array(decodeItem), path)
  };
};
var decodeProgress = (v, path) => ({
  status: field(object(v, path), "status", string, path)
});
function next(status) {
  if (!status.done)
    return { poll: true };
  if (status.error !== null)
    return { error: status.error };
  if (status.no_connection !== null)
    return { noConnection: status.no_connection };
  return { plot: true };
}

// progress.ts
var log = byId("log", HTMLPreElement);
var now = byId("now", HTMLPreElement);
var title = byId("title", HTMLHeadingElement);
var back = byId("back", HTMLAnchorElement);
var { status: statusUrl } = decodeProgress(parseJson(byId("data", HTMLScriptElement).text), "data");
var POLL_MS = 1000;
var SPREAD_MS = 900;
var RING_MS = 1800;
var GLOW_MS = 1500;
var FADE_MS = 600;
var MORPH_MS = 1800;
var CUT = 0.02;
var SAME_KM = 1;
var calm = matchMedia("(prefers-reduced-motion: reduce)").matches;
var trainsCanvas = byId("sketch", HTMLCanvasElement);
var asksCanvas = byId("asks", HTMLCanvasElement);
var trainsCtx = trainsCanvas.getContext("2d");
var asksCtx = asksCanvas.getContext("2d");
var picture = emptyPicture();
var queue = [];
var queued = 0;
var rate = 0;
var budget = 0;
var received = 0;
var rings = [];
var bands = [];
var glows = [];
var born = new Map;
var ghosts = null;
var morphT0 = null;
var morphFrom = [];
var stationAt = new Map;
var tShown = null;
var yShown = null;
var tTarget = null;
var yTarget = null;
var rangesDue = true;
var rangesAt = 0;
var axisOf = new WeakMap;
function kmLeft(s) {
  const ends = picture.ends;
  if (ends === null)
    return null;
  let h = axisOf.get(s);
  if (h === undefined)
    axisOf.set(s, h = axis(s.at, ends));
  return h;
}
var ease = (x) => x < 0.5 ? 4 * x * x * x : 1 - (-2 * x + 2) ** 3 / 2;
var clamp01 = (x) => Math.min(1, Math.max(0, x));
var morph = (t) => morphT0 === null ? 0 : calm ? 1 : ease(clamp01((t - morphT0) / MORPH_MS));
function stationHeight(i, e) {
  const s = picture.timetable?.stations[i];
  if (s === undefined || s.height === null)
    return null;
  const from = morphFrom[i] ?? null;
  return from === null ? s.height : from + (s.height - from) * e;
}
var fromKm = (km2, kmRange, range) => range[0] + (km2 - kmRange[0]) / (kmRange[1] - kmRange[0] || 1) * (range[1] - range[0]);
function provisional() {
  const lines = new Map;
  for (const [id, stops] of picture.trains) {
    const line = corners(stops.map((s) => [kmLeft(s), s.arrival, s.departure]));
    if (line.length > 0)
      lines.set(id, line);
  }
  return lines;
}
function timetabled(e) {
  const tt = picture.timetable;
  if (tt === null)
    return [];
  const heights = tt.stations.map((_, i) => stationHeight(i, e));
  return tt.trains.map((calls) => corners(calls.map(([s, a, d]) => [heights[s] ?? null, a, d])));
}
function targetRanges() {
  const ends = picture.ends;
  const times = [];
  const heights = [];
  const lines = picture.timetable ? timetabled(1) : [...provisional().values()];
  for (const line of lines) {
    for (const [t, h] of line) {
      times.push(t);
      heights.push(h);
    }
  }
  if (times.length === 0 && ends === null)
    return;
  const window2 = ends ? [ends.depart_from, ends.depart_until] : [Infinity, -Infinity];
  tTarget = spread(times, CUT, window2);
  const goal = picture.timetable ? [0, 0] : ends ? [0, axis(ends.from.at, ends)] : [Infinity, -Infinity];
  yTarget = spread(heights, picture.timetable ? 0 : CUT, goal);
  if (yTarget[1] <= yTarget[0])
    yTarget = [yTarget[0], yTarget[0] + 1];
  if (tTarget[1] <= tTarget[0])
    tTarget = [tTarget[0], tTarget[0] + 3600];
}
function approach(shown, target, dt) {
  if (shown === null || calm)
    return [target, false];
  const k = 1 - Math.exp(-dt / 400);
  const moved = [shown[0] + (target[0] - shown[0]) * k, shown[1] + (target[1] - shown[1]) * k];
  const span = target[1] - target[0];
  const still = Math.abs(moved[0] - target[0]) < span * 0.001 && Math.abs(moved[1] - target[1]) < span * 0.001;
  return [still ? target : moved, !still];
}
var W = 0;
var H = 0;
var dpr = 1;
function resize() {
  dpr = window.devicePixelRatio || 1;
  W = window.innerWidth;
  H = window.innerHeight;
  for (const c of [trainsCanvas, asksCanvas]) {
    c.width = Math.round(W * dpr);
    c.height = Math.round(H * dpr);
  }
  wake();
}
var mx = () => Math.max(24, W * 0.04);
var my = () => Math.max(32, H * 0.08);
function sx(t) {
  if (tShown === null)
    return 0;
  return mx() + (t - tShown[0]) / (tShown[1] - tShown[0]) * (W - 2 * mx());
}
function sy(h, range = yShown) {
  if (range === null)
    return 0;
  return H - my() - (h - range[0]) / (range[1] - range[0]) * (H - 2 * my());
}
var colors = { accent: "#d97706", grid: "#e5e7eb", fg: "#1d2129" };
function readColors() {
  const css = getComputedStyle(document.body);
  const v = (name, fallback) => css.getPropertyValue(name).trim() || fallback;
  colors = { accent: v("--accent", colors.accent), grid: v("--grid", colors.grid), fg: v("--fg", colors.fg) };
  wake();
}
var hue = (n) => n * 137.508 % 360;
var trainColor = (n) => `hsl(${String(hue(n))}, 70%, 50%)`;
function idNumber(id) {
  let h = 0;
  for (let i = 0;i < id.length; i++)
    h = Math.imul(h, 31) + id.charCodeAt(i) | 0;
  return Math.abs(h);
}
function stroke(ctx, line, y) {
  ctx.beginPath();
  line.forEach(([t, h], k) => {
    if (k === 0)
      ctx.moveTo(sx(t), sy(h, y));
    else
      ctx.lineTo(sx(t), sy(h, y));
  });
  ctx.stroke();
}
function rowOf(at2, e) {
  const tt = picture.timetable;
  if (tt) {
    const key = at2.join();
    let i = stationAt.get(key);
    if (i === undefined)
      stationAt.set(key, i = nearest(tt.stations, at2, SAME_KM));
    return i === null ? null : stationHeight(i, e);
  }
  return picture.ends ? axis(at2, picture.ends) : null;
}
function drawTrains(t) {
  const ctx = trainsCtx;
  if (ctx === null || tShown === null || yShown === null)
    return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, W, H);
  ctx.lineJoin = "round";
  ctx.lineCap = "round";
  const e = morph(t);
  const stations = picture.timetable?.stations ?? [];
  ctx.strokeStyle = colors.grid;
  ctx.lineWidth = 1;
  ctx.globalAlpha = calm ? 1 : clamp01((t - (morphT0 ?? t)) / FADE_MS);
  ctx.beginPath();
  for (const i of stations.keys()) {
    const h = stationHeight(i, e);
    if (h === null)
      continue;
    ctx.moveTo(mx(), sy(h));
    ctx.lineTo(W - mx(), sy(h));
  }
  ctx.stroke();
  ctx.fillStyle = colors.accent;
  ctx.globalAlpha = 0.07;
  for (const b of bands) {
    const h = rowOf(b.at, e);
    if (h === null)
      continue;
    const x0 = sx(b.span[0]);
    ctx.fillRect(x0, sy(h) - 5, sx(b.span[1]) - x0, 10);
  }
  ctx.lineWidth = 1.4;
  if (ghosts) {
    const fade = calm ? 0 : 1 - clamp01((t - (morphT0 ?? t)) / (MORPH_MS * 0.5));
    ghosts.lines.forEach((line, k) => {
      ctx.globalAlpha = 0.8 * fade;
      ctx.strokeStyle = trainColor(idNumber(ghosts?.ids[k] ?? ""));
      stroke(ctx, line, ghosts?.y);
    });
  }
  if (picture.timetable) {
    const fadeIn = calm ? 1 : clamp01((t - (morphT0 ?? t)) / FADE_MS);
    timetabled(e).forEach((line, k) => {
      if (line.length === 0)
        return;
      ctx.globalAlpha = 0.85 * fadeIn;
      ctx.strokeStyle = trainColor(k);
      stroke(ctx, line);
    });
  } else {
    for (const [id, line] of provisional()) {
      let b = born.get(id);
      if (b === undefined)
        born.set(id, b = t);
      ctx.globalAlpha = 0.85 * (calm ? 1 : clamp01((t - b) / FADE_MS));
      ctx.strokeStyle = trainColor(idNumber(id));
      stroke(ctx, line);
    }
  }
  ctx.globalAlpha = 1;
}
function drawAsks(t) {
  const ctx = asksCtx;
  if (ctx === null)
    return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, W, H);
  if (tShown === null || yShown === null)
    return;
  const e = morph(t);
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  const lines = glows.length > 0 ? timetabled(e) : [];
  for (const g of glows) {
    const line = lines[g.trip];
    if (!line || line.length === 0)
      continue;
    const a = clamp01((t - g.t0) / GLOW_MS);
    ctx.globalAlpha = 0.9 * (1 - a);
    ctx.strokeStyle = colors.accent;
    ctx.lineWidth = 3.5;
    stroke(ctx, line);
  }
  ctx.font = "12px system-ui, sans-serif";
  ctx.textBaseline = "middle";
  const named = new Set;
  const names = new Set;
  for (const r of rings.toReversed()) {
    if (names.size === 3)
      break;
    if (!names.has(r.place.name))
      named.add(r);
    names.add(r.place.name);
  }
  for (const r of rings) {
    const h = rowOf(r.place.at, e);
    if (h === null)
      continue;
    const a = clamp01((t - r.t0) / RING_MS);
    const y = sy(h);
    const x = r.span ? sx((r.span[0] + r.span[1]) / 2) : mx();
    ctx.globalAlpha = 1 - a;
    ctx.strokeStyle = colors.accent;
    ctx.lineWidth = 2;
    if (r.span) {
      const x0 = sx(r.span[0]);
      ctx.beginPath();
      ctx.roundRect(x0, y - 7, Math.max(sx(r.span[1]) - x0, 14), 14, 7);
      ctx.stroke();
    }
    const radius = calm ? 12 : 6 + 20 * (1 - (1 - a) ** 3);
    ctx.beginPath();
    ctx.arc(x, y, radius, 0, 2 * Math.PI);
    ctx.stroke();
    ctx.fillStyle = colors.accent;
    ctx.beginPath();
    ctx.arc(x, y, 3, 0, 2 * Math.PI);
    ctx.fill();
    if (named.has(r)) {
      ctx.globalAlpha = (1 - a) * 0.9;
      ctx.fillStyle = colors.fg;
      ctx.fillText(r.place.name, x + radius + 6, y);
    }
  }
  ctx.globalAlpha = 1;
}
function take(it, t) {
  if (it.kind === "timetable") {
    const lines = provisional();
    ghosts = { lines: [...lines.values()], ids: [...lines.keys()], y: yShown ?? yTarget ?? [0, 1] };
  }
  const effect = apply(picture, it);
  rangesDue = true;
  if (effect === null)
    return;
  if ("asked" in effect) {
    rings.push({ place: effect.asked, span: effect.span, t0: t });
    if (effect.span)
      bands.push({ at: effect.asked.at, span: effect.span });
  } else if ("askedTrain" in effect) {
    glows.push({ trip: effect.askedTrain, t0: t });
  } else {
    morphT0 = t;
    targetRanges();
    yShown = yTarget;
    const [ends, from, to] = [picture.ends, ghosts?.y, yTarget];
    morphFrom = (picture.timetable?.stations ?? []).map((s) => s.at && ends && from && to ? fromKm(axis(s.at, ends), from, to) : null);
    rangesAt = t;
    rangesDue = false;
  }
}
var running = false;
var last = 0;
function frame(t) {
  const dt = Math.max(0, Math.min(100, t - last));
  last = t;
  budget = calm ? queue.length - queued : budget + rate * dt;
  while (queued < queue.length && budget >= 1) {
    const it = queue[queued++];
    if (it)
      take(it, t);
    budget--;
  }
  if (queued >= queue.length) {
    queue.length = queued = 0;
    budget = 0;
  }
  const morphing = morphT0 !== null && t - morphT0 < MORPH_MS;
  if (afterDrawn && queue.length === 0 && !morphing) {
    const then = afterDrawn;
    afterDrawn = null;
    then();
  }
  if (rangesDue && t - rangesAt > 250) {
    targetRanges();
    rangesAt = t;
    rangesDue = false;
  }
  let moving = false;
  if (tTarget && yTarget) {
    let m1, m2;
    [tShown, m1] = approach(tShown, tTarget, dt);
    [yShown, m2] = approach(yShown, yTarget, dt);
    moving = m1 || m2;
  }
  while (rings.length > 0 && t - (rings[0]?.t0 ?? t) > RING_MS)
    rings.shift();
  while (glows.length > 0 && t - (glows[0]?.t0 ?? t) > GLOW_MS)
    glows.shift();
  if (ghosts && morphT0 !== null && t - morphT0 > MORPH_MS)
    ghosts = null;
  const fading = [...born.values()].some((b) => t - b < FADE_MS);
  drawTrains(t);
  drawAsks(t);
  const busy = queued < queue.length || rangesDue || moving || fading || rings.length > 0 || glows.length > 0 || morphing;
  if (busy)
    requestAnimationFrame(frame);
  else
    running = false;
}
function wake() {
  if (running)
    return;
  running = true;
  last = performance.now();
  requestAnimationFrame(frame);
}
function enqueue(items) {
  if (items.length === 0)
    return;
  queue.push(...items);
  rate = (queue.length - queued) / SPREAD_MS;
  wake();
}
var afterDrawn = null;
addEventListener("resize", resize);
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", readColors);
resize();
readColors();
async function poll() {
  let status;
  try {
    const response = await fetch(`${statusUrl}?sketch=${String(received)}`);
    if (response.status === 404) {
      document.body.classList.add("over");
      title.textContent = "Search not found";
      return;
    }
    if (!response.ok)
      throw new Error(`status ${String(response.status)}`);
    status = decodeStatus(parseJson(await response.text()), "status");
  } catch {
    setTimeout(() => void poll(), POLL_MS);
    return;
  }
  received += status.sketch.length;
  enqueue(status.sketch);
  back.href = status.back;
  log.textContent = status.log.join(`
`);
  now.textContent = status.now ?? "";
  const then = next(status);
  if ("poll" in then) {
    setTimeout(() => void poll(), POLL_MS);
    return;
  }
  if ("plot" in then) {
    let reloading = false;
    const reload = () => {
      if (reloading)
        return;
      reloading = true;
      location.reload();
    };
    if (document.hidden) {
      reload();
      return;
    }
    afterDrawn = reload;
    setTimeout(reload, SPREAD_MS + MORPH_MS + 500);
    wake();
    return;
  }
  document.body.classList.add("over");
  const [heading, className, text] = "error" in then ? ["No plot", "error", then.error] : ["No connection found", "report", then.noConnection];
  title.textContent = heading;
  log.after(Object.assign(document.createElement("pre"), { className, textContent: text }));
}
poll();
