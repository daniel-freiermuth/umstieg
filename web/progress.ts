// Progress page of a running search (`umstieg serve`), shown at the search's
// own link: polls the job's status and loads the link again when the plot is
// ready, which then shows it. Behind the log, blurred, it draws what the
// search has seen so far (sketch.ts): trains as the answers name them,
// stations circled as they are asked about, and once built, the timetable
// gliding onto its final heights.

import { parseJson } from "./decode";
import { byId } from "./dom";
import {
  type Corner,
  type Item,
  type LatLon,
  type Place,
  type Stop,
  apply,
  axis,
  corners,
  emptyPicture,
  nearest,
  spread,
} from "./sketch";
import { type Status, decodeProgress, decodeStatus, next } from "./status";

const log = byId("log", HTMLPreElement);
const now = byId("now", HTMLPreElement);
const title = byId("title", HTMLHeadingElement);
const back = byId("back", HTMLAnchorElement);
const { status: statusUrl } = decodeProgress(parseJson(byId("data", HTMLScriptElement).text), "data");

// ---- The sketch -----------------------------------------------------------

const POLL_MS = 1000;
/** A poll's items are drawn one by one over this long, so each shows. */
const SPREAD_MS = 900;
const RING_MS = 1800;
const GLOW_MS = 1500;
const FADE_MS = 600;
const MORPH_MS = 1800;
/** Share of times and heights cut at either end of the drawn range. */
const CUT = 0.02;
/** An asked station this close to a timetable station is that one. */
const SAME_KM = 1;

const calm = matchMedia("(prefers-reduced-motion: reduce)").matches;
const trainsCanvas = byId("sketch", HTMLCanvasElement);
const asksCanvas = byId("asks", HTMLCanvasElement);
const trainsCtx = trainsCanvas.getContext("2d");
const asksCtx = asksCanvas.getContext("2d");

const picture = emptyPicture();
const queue: Item[] = [];
let queued = 0; // next item of `queue` to apply
let rate = 0; // items per ms
let budget = 0;
/** Items received so far: the next poll asks from there on. */
let received = 0;

/** A station asked about, circled for a while; its span stays tinted. */
interface Ring {
  place: Place;
  span: [number, number] | null;
  t0: number;
}
const rings: Ring[] = [];
const bands: { at: LatLon; span: [number, number] }[] = [];
const glows: { trip: number; t0: number }[] = [];
/** When each train was first drawn, for fading it in. */
const born = new Map<string, number>();
/** The trains before the timetable came, fading out, at the heights they had. */
let ghosts: { lines: [number, number][][]; ids: string[]; y: [number, number] } | null = null;
let morphT0: number | null = null;
/** Where each timetable station starts its move: its distance left put at the same place among the minutes. */
let morphFrom: (number | null)[] = [];
/** The timetable station an asked place is drawn at, by position. */
const stationAt = new Map<string, number | null>();

// Shown range: times (Unix seconds) and heights (km left until the timetable,
// then remaining minutes), easing towards their targets.
let tShown: [number, number] | null = null;
let yShown: [number, number] | null = null;
let tTarget: [number, number] | null = null;
let yTarget: [number, number] | null = null;
let rangesDue = true;
let rangesAt = 0;

const axisOf = new WeakMap<Stop, number>();
function kmLeft(s: Stop): number | null {
  const ends = picture.ends;
  if (ends === null) return null;
  let h = axisOf.get(s);
  if (h === undefined) axisOf.set(s, (h = axis(s.at, ends)));
  return h;
}

const ease = (x: number): number => (x < 0.5 ? 4 * x * x * x : 1 - (-2 * x + 2) ** 3 / 2);
const clamp01 = (x: number): number => Math.min(1, Math.max(0, x));
/** The timetable's move onto its heights, 0..1. */
const morph = (t: number): number => (morphT0 === null ? 0 : calm ? 1 : ease(clamp01((t - morphT0) / MORPH_MS)));

/** Height of timetable station `i` as drawn at morph progress `e`, in the timetable's minutes; `null`: not drawn. */
function stationHeight(i: number, e: number): number | null {
  const s = picture.timetable?.stations[i];
  if (s === undefined || s.height === null) return null;
  const from = morphFrom[i] ?? null;
  return from === null ? s.height : from + (s.height - from) * e;
}
/** Height `km` in the range `kmRange`, put at the same place in `range`. */
const fromKm = (km: number, kmRange: [number, number], range: [number, number]): number =>
  range[0] + ((km - kmRange[0]) / (kmRange[1] - kmRange[0] || 1)) * (range[1] - range[0]);

/** Each provisional train's drawn corners, by id. */
function provisional(): Map<string, [number, number][]> {
  const lines = new Map<string, [number, number][]>();
  for (const [id, stops] of picture.trains) {
    const line = corners(stops.map((s): Corner => [kmLeft(s), s.arrival, s.departure]));
    if (line.length > 0) lines.set(id, line);
  }
  return lines;
}
/** Each timetable train's drawn corners at morph progress `e`. */
function timetabled(e: number): [number, number][][] {
  const tt = picture.timetable;
  if (tt === null) return [];
  const heights = tt.stations.map((_, i) => stationHeight(i, e));
  return tt.trains.map((calls) => corners(calls.map(([s, a, d]): Corner => [heights[s] ?? null, a, d])));
}

/** Targets of the shown range from what is drawn. */
function targetRanges(): void {
  const ends = picture.ends;
  const times: number[] = [];
  const heights: number[] = [];
  const lines = picture.timetable ? timetabled(1) : [...provisional().values()];
  for (const line of lines) {
    for (const [t, h] of line) {
      times.push(t);
      heights.push(h);
    }
  }
  if (times.length === 0 && ends === null) return;
  const window: [number, number] = ends ? [ends.depart_from, ends.depart_until] : [Infinity, -Infinity];
  tTarget = spread(times, CUT, window);
  const goal: [number, number] = picture.timetable
    ? [0, 0]
    : ends
      ? [0, axis(ends.from.at, ends)]
      : [Infinity, -Infinity];
  yTarget = spread(heights, picture.timetable ? 0 : CUT, goal);
  if (yTarget[1] <= yTarget[0]) yTarget = [yTarget[0], yTarget[0] + 1];
  if (tTarget[1] <= tTarget[0]) tTarget = [tTarget[0], tTarget[0] + 3600];
}

/** `shown` eased towards `target` over `dt` ms; whether it is still moving. */
function approach(shown: [number, number] | null, target: [number, number], dt: number): [[number, number], boolean] {
  if (shown === null || calm) return [target, false];
  const k = 1 - Math.exp(-dt / 400);
  const moved: [number, number] = [shown[0] + (target[0] - shown[0]) * k, shown[1] + (target[1] - shown[1]) * k];
  const span = target[1] - target[0];
  const still = Math.abs(moved[0] - target[0]) < span * 1e-3 && Math.abs(moved[1] - target[1]) < span * 1e-3;
  return [still ? target : moved, !still];
}

// Where on the screen: the plot area with a margin, the goal at the bottom.
let W = 0,
  H = 0,
  dpr = 1;
function resize(): void {
  dpr = window.devicePixelRatio || 1;
  W = window.innerWidth;
  H = window.innerHeight;
  for (const c of [trainsCanvas, asksCanvas]) {
    c.width = Math.round(W * dpr);
    c.height = Math.round(H * dpr);
  }
  wake();
}
const mx = (): number => Math.max(24, W * 0.04);
const my = (): number => Math.max(32, H * 0.08);
function sx(t: number): number {
  if (tShown === null) return 0;
  return mx() + ((t - tShown[0]) / (tShown[1] - tShown[0])) * (W - 2 * mx());
}
function sy(h: number, range: [number, number] | null = yShown): number {
  if (range === null) return 0;
  return H - my() - ((h - range[0]) / (range[1] - range[0])) * (H - 2 * my());
}

let colors = { accent: "#d97706", grid: "#e5e7eb", fg: "#1d2129" };
function readColors(): void {
  const css = getComputedStyle(document.body);
  const v = (name: string, fallback: string): string => css.getPropertyValue(name).trim() || fallback;
  colors = { accent: v("--accent", colors.accent), grid: v("--grid", colors.grid), fg: v("--fg", colors.fg) };
  wake();
}
// The plot's colours: one per train.
const hue = (n: number): number => (n * 137.508) % 360;
const trainColor = (n: number): string => `hsl(${String(hue(n))}, 70%, 50%)`;
function idNumber(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (Math.imul(h, 31) + id.charCodeAt(i)) | 0;
  return Math.abs(h);
}

function stroke(ctx: CanvasRenderingContext2D, line: readonly [number, number][], y?: [number, number]): void {
  ctx.beginPath();
  line.forEach(([t, h], k) => {
    if (k === 0) ctx.moveTo(sx(t), sy(h, y));
    else ctx.lineTo(sx(t), sy(h, y));
  });
  ctx.stroke();
}

/** The row an asked place is drawn on, in the shown heights; `null` if it has none yet. */
function rowOf(at: LatLon, e: number): number | null {
  const tt = picture.timetable;
  if (tt) {
    const key = at.join();
    let i = stationAt.get(key);
    if (i === undefined) stationAt.set(key, (i = nearest(tt.stations, at, SAME_KM)));
    return i === null ? null : stationHeight(i, e);
  }
  return picture.ends ? axis(at, picture.ends) : null;
}

function drawTrains(t: number): void {
  const ctx = trainsCtx;
  if (ctx === null || tShown === null || yShown === null) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, W, H);
  ctx.lineJoin = "round";
  ctx.lineCap = "round";
  const e = morph(t);

  // The timetable's stations as the plot's rows.
  const stations = picture.timetable?.stations ?? [];
  ctx.strokeStyle = colors.grid;
  ctx.lineWidth = 1;
  ctx.globalAlpha = calm ? 1 : clamp01((t - (morphT0 ?? t)) / FADE_MS);
  ctx.beginPath();
  for (const i of stations.keys()) {
    const h = stationHeight(i, e);
    if (h === null) continue;
    ctx.moveTo(mx(), sy(h));
    ctx.lineTo(W - mx(), sy(h));
  }
  ctx.stroke();

  // What the boards asked covered, faintly.
  ctx.fillStyle = colors.accent;
  ctx.globalAlpha = 0.07;
  for (const b of bands) {
    const h = rowOf(b.at, e);
    if (h === null) continue;
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
      if (line.length === 0) return;
      ctx.globalAlpha = 0.85 * fadeIn;
      ctx.strokeStyle = trainColor(k);
      stroke(ctx, line);
    });
  } else {
    for (const [id, line] of provisional()) {
      let b = born.get(id);
      if (b === undefined) born.set(id, (b = t));
      ctx.globalAlpha = 0.85 * (calm ? 1 : clamp01((t - b) / FADE_MS));
      ctx.strokeStyle = trainColor(idNumber(id));
      stroke(ctx, line);
    }
  }
  ctx.globalAlpha = 1;
}

function drawAsks(t: number): void {
  const ctx = asksCtx;
  if (ctx === null) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, W, H);
  if (tShown === null || yShown === null) return;
  const e = morph(t);

  // Timetable trains asked about glow for a moment.
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  const lines = glows.length > 0 ? timetabled(e) : [];
  for (const g of glows) {
    const line = lines[g.trip];
    if (!line || line.length === 0) continue;
    const a = clamp01((t - g.t0) / GLOW_MS);
    ctx.globalAlpha = 0.9 * (1 - a);
    ctx.strokeStyle = colors.accent;
    ctx.lineWidth = 3.5;
    stroke(ctx, line);
  }

  // Stations asked about: a ring spreading and fading, the asked span
  // outlined, and the newest few stations named, each once.
  ctx.font = "12px system-ui, sans-serif";
  ctx.textBaseline = "middle";
  const named = new Set<Ring>();
  const names = new Set<string>();
  for (const r of rings.toReversed()) {
    if (names.size === 3) break;
    if (!names.has(r.place.name)) named.add(r);
    names.add(r.place.name);
  }
  for (const r of rings) {
    const h = rowOf(r.place.at, e);
    if (h === null) continue;
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

function take(it: Item, t: number): void {
  if (it.kind === "timetable") {
    const lines = provisional();
    ghosts = { lines: [...lines.values()], ids: [...lines.keys()], y: yShown ?? yTarget ?? [0, 1] };
  }
  const effect = apply(picture, it);
  rangesDue = true;
  if (effect === null) return;
  if ("asked" in effect) {
    rings.push({ place: effect.asked, span: effect.span, t0: t });
    if (effect.span) bands.push({ at: effect.asked.at, span: effect.span });
  } else if ("askedTrain" in effect) {
    glows.push({ trip: effect.askedTrain, t0: t });
  } else {
    morphT0 = t;
    // The heights' range jumps from km to minutes: start there.
    targetRanges();
    yShown = yTarget;
    const [ends, from, to] = [picture.ends, ghosts?.y, yTarget];
    morphFrom = (picture.timetable?.stations ?? []).map((s) =>
      s.at && ends && from && to ? fromKm(axis(s.at, ends), from, to) : null,
    );
    rangesAt = t;
    rangesDue = false;
  }
}

let running = false;
let last = 0;
function frame(t: number): void {
  // A frame's timestamp can precede the clock read when it was asked for.
  const dt = Math.max(0, Math.min(100, t - last));
  last = t;
  // A poll's items, spread over the time until the next; all at once when calm.
  budget = calm ? queue.length - queued : budget + rate * dt;
  while (queued < queue.length && budget >= 1) {
    const it = queue[queued++];
    if (it) take(it, t);
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
  while (rings.length > 0 && t - (rings[0]?.t0 ?? t) > RING_MS) rings.shift();
  while (glows.length > 0 && t - (glows[0]?.t0 ?? t) > GLOW_MS) glows.shift();
  if (ghosts && morphT0 !== null && t - morphT0 > MORPH_MS) ghosts = null;
  const fading = [...born.values()].some((b) => t - b < FADE_MS);
  drawTrains(t);
  drawAsks(t);
  const busy =
    queued < queue.length || rangesDue || moving || fading || rings.length > 0 || glows.length > 0 || morphing;
  if (busy) requestAnimationFrame(frame);
  else running = false;
}
function wake(): void {
  if (running) return;
  running = true;
  last = performance.now();
  requestAnimationFrame(frame);
}
function enqueue(items: readonly Item[]): void {
  if (items.length === 0) return;
  queue.push(...items);
  rate = (queue.length - queued) / SPREAD_MS;
  wake();
}
/** Called once every received item is drawn and the timetable has moved onto its heights. */
let afterDrawn: (() => void) | null = null;

addEventListener("resize", resize);
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", readColors);
resize();
readColors();

// ---- The log --------------------------------------------------------------

async function poll(): Promise<void> {
  let status: Status;
  try {
    const response = await fetch(`${statusUrl}?sketch=${String(received)}`);
    if (response.status === 404) {
      document.body.classList.add("over");
      title.textContent = "Search not found";
      return;
    }
    // The server answers only 200 or 404: any other failure is its proxy's (a
    // 502 or 504 while the server is slow), and the search runs on.
    if (!response.ok) throw new Error(`status ${String(response.status)}`);
    status = decodeStatus(parseJson(await response.text()), "status");
  } catch {
    // No status this time (a network blip, the laptop asleep, a proxy's page
    // or error instead): the search runs on, so ask again.
    setTimeout(() => void poll(), POLL_MS);
    return;
  }
  received += status.sketch.length;
  enqueue(status.sketch);
  back.href = status.back;
  log.textContent = status.log.join("\n");
  // The current stage's progress, under the log, replaced as it comes.
  now.textContent = status.now ?? "";
  const then = next(status);
  if ("poll" in then) {
    setTimeout(() => void poll(), POLL_MS);
    return;
  }
  if ("plot" in then) {
    // What is still being drawn first: at most a poll's spread and the
    // timetable's move. A hidden page draws nothing (no frames), so it loads
    // the plot at once, and a drawing page within that time at the latest.
    let reloading = false;
    const reload = (): void => {
      if (reloading) return;
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
  const [heading, className, text] =
    "error" in then ? ["No plot", "error", then.error] : ["No connection found", "report", then.noConnection];
  title.textContent = heading;
  log.after(Object.assign(document.createElement("pre"), { className, textContent: text }));
}

void poll();
