import {
  type Detail,
  type DetailData,
  type Fallback,
  type Piece,
  type Row,
  addDetails,
  applyHeights,
  decodeData,
  decodeDetails,
  extent,
  load,
} from "./data";
import { parseJson } from "./decode";
import { byId } from "./dom";
import {
  type Domain,
  type Finger,
  type Geom,
  type Pt,
  type XY,
  clipLine,
  distanceToLine,
  pan,
  pinch,
  spreadLabels,
  tickStep,
  zoom,
} from "./geometry";
import { type Candidates, type Picked, narrow, pinAfter, step, toggle } from "./picks";
import { type Reach, type Ride, network, runsOf, stayAt } from "./reach";
import { entry, item } from "./util";

// What the mouse is over: a line, a station, a fallback.
interface Hover {
  info: string;
  chains: number[];
  pts: XY[];
  trip?: number;
  reach?: Reach;
  fallbacks?: Fallback[];
}
interface Target extends Hover {
  id: string;
}
interface Edge extends Ride {
  info: string;
}
interface HitTarget {
  pts: XY[];
  at: (time: number) => Target;
}
interface FbTarget {
  at: (time: number) => Hover;
}
interface Model {
  edges: Edge[];
  waits: number[];
  walks: number[];
  targets: HitTarget[];
  stationTarget: (s: number) => Target;
}
interface View {
  svg: SVGSVGElement;
  x: (t: number) => number;
  y: (v: number) => number;
  pts: (p: { pts: readonly XY[] }) => string;
  overlay: SVGGElement;
  hoverLayer: SVGGElement;
  fbLayer: SVGGElement;
  labels: SVGGElement;
  overlayKey: string | null;
  fbKey: number | null | undefined;
  fbTargets: FbTarget[];
}

const D = load(decodeData(parseJson(byId("data", HTMLScriptElement).text), "data"));

// Connections' routes and fallbacks: in the page of a plot file, otherwise
// asked for when shown (`umstieg serve` keeps them; a night search can list
// 140 000 connections, which would make the page huge).
const DETAILS_AT_ONCE = 500;
/** Connections listed in the panel at most. */
const MAX_LISTED = 200;
const asked = new Set<number>(),
  lost = new Set<number>();
/** A connection's route, or what stands in for it until (unless) it arrives. */
const routeOf = (i: number): string =>
  D.details.get(i)?.route ?? (lost.has(i) ? "(details gone: the search has expired, run it again)" : "…");
/** The connection's details if known; otherwise asks for them and redraws once they arrive. */
function detailOf(i: number): Detail | undefined {
  const known = D.details.get(i);
  if (known === undefined) want([i]);
  return known;
}
function want(chains: readonly number[]): void {
  const missing = chains.filter((i) => !D.details.has(i) && !asked.has(i));
  for (let at = 0; at < missing.length; at += DETAILS_AT_ONCE) {
    const ids = missing.slice(at, at + DETAILS_AT_ONCE);
    for (const i of ids) asked.add(i);
    void (async () => {
      try {
        const response = D.details_url === null ? null : await fetch(`${D.details_url}?ids=${ids.join(",")}`);
        // Asked once only: a plot whose search is gone has no details left to give.
        if (response?.ok) {
          const got = decodeDetails(parseJson(await response.text()), "details");
          addDetails(
            D,
            got.map((d, k): [number, DetailData] => [item(ids, k), d]),
          );
        } else for (const i of ids) lost.add(i);
      } catch {
        // Failed (a network blip, the laptop asleep) or answered with something
        // else (a proxy's page): asked again when shown again.
        for (const i of ids) asked.delete(i);
        return;
      }
      panelKey = null;
      if (view) view.fbKey = undefined;
      refresh();
    })();
  }
}

const NS = "http://www.w3.org/2000/svg";
const plotEl = byId("plot", HTMLDivElement);
const tip = byId("tip", HTMLDivElement);
const details = byId("details", HTMLDivElement);
const panelTitle = byId("panel-title", HTMLHeadingElement);
const picksEl = byId("picks", HTMLDivElement);
// Axis ticks: the legend names the zone. Times in texts carry it themselves ("08:10 CET").
const tickFmt = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
  timeZone: D.tz,
});
const timeFmt = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
  timeZone: D.tz,
  timeZoneName: "short",
});
// One colour per train, so a train can be followed across all connections.
const trainColor = (t: number): string => `hsl(${String((t * 137.508) % 360)}, 70%, 45%)`;
const HINT =
  "Drawn is everything on some connection within the limits; listed are the best connections. Hover a line " +
  "or station name to see what is reachable through it. Click it to pick it: the listed connections passing " +
  "it are shown (one alone is selected). Click more to narrow down, a pick again to remove it.";

// Interaction state. Selection only changes on click; hovering only previews.
const hidden = new Set<number>(); // hidden groups
let pinned: number | null = null; // selected connection
let picks: Picked[] = []; // clicked lines/stations narrowing the list
let candidates: Candidates | null = null; // connections passing every pick
let hoverTarget: Hover | null = null; // line under the mouse
let preview: number | null = null; // connection hovered in the candidate list
let softGroup: number | null = null; // group hovered in the group list

byId("title", HTMLHeadingElement).textContent = D.title;
byId("subtitle", HTMLParagraphElement).textContent = D.subtitle;
if (D.back) Object.assign(byId("new-search", HTMLAnchorElement), { href: D.back, hidden: false });
const legend = byId("legend", HTMLDivElement);
function writeLegend(): void {
  const metric = item(D.metrics, metricIndex);
  legend.innerHTML =
    `x: time (${D.tz}). y: ${metric.description} (${metric.unit}), goal at the bottom.<br>` +
    `Coloured: riding, one colour per train · thin grey: waiting (red: below minimum transfer, or missed by live data ✗) · dotted: walking.<br>` +
    `Parallel lines: same route later. Steeper: faster. Dashed: fallbacks of the selection (orange dots: waiting).<br>` +
    `<kbd>↑</kbd> <kbd>↓</kbd> step through listed connections, <kbd>Esc</kbd> clears the selection.<br>` +
    `Wheel: zoom (<kbd>Shift</kbd> time only, <kbd>Alt</kbd> height only) · drag: pan · two fingers: zoom (sideways time, up and down height) · double-click: reset.`;
}

/** `text` in `t`, cut with "…" to at most `width` pixels; the whole name as its tooltip. */
function fitText(t: SVGTextElement, text: string, width: number): void {
  t.textContent = text;
  const whole = t.getComputedTextLength();
  if (whole <= width) return;
  // Measuring costs a layout: start from the proportional guess, then shorten.
  let keep = Math.max(1, Math.floor((text.length * width) / whole));
  t.textContent = text.slice(0, keep).trimEnd() + "…";
  while (keep > 1 && t.getComputedTextLength() > width) t.textContent = text.slice(0, --keep).trimEnd() + "…";
  el("title", {}, t).textContent = text;
}

function el<K extends keyof SVGElementTagNameMap>(
  name: K,
  attrs: Record<string, string | number>,
  parent?: Element,
): SVGElementTagNameMap[K] {
  const e = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, String(v));
  if (parent) parent.appendChild(e);
  return e;
}
const ESCAPES: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" };
function esc(s: string): string {
  return s.replace(/[&<>"]/g, (ch) => ESCAPES[ch] ?? ch);
}

let metricIndex = D.metric;
let rows: Row[] = []; // stations with a row, top to bottom
let full: Domain = { t0: Infinity, t1: -Infinity, y0: 0, y1: 0 };
let dom: Domain = { ...full }; // visible part: zoomed and panned
let model: Model | null = null; // shared stretches of the visible connections
let view: View | null = null; // per-render drawing context
let geom: Geom | null = null; // plot area of the last render

// Height of station s per the chosen metric, null if it has none.
function heightOf(s: number): number | null {
  return item(D.metrics, metricIndex).heights[s] ?? null;
}
// Heights per the chosen metric; zoom resets to the full extent.
function applyMetric(k: number): void {
  metricIndex = k;
  rows = applyHeights(D, k);
  full = extent(D);
  dom = { ...full };
  writeLegend();
}
const metricSelect = byId("metric", HTMLSelectElement);
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

// The network's smallest units (reach.ts), each drawn once and used as hover
// target; stays are resolved by the time under the mouse. Depends only on
// the heights and which groups are hidden, not on zoom.
function buildModel(): void {
  const net = network(D.pieces, D.network, D.chains, (i) => !hidden.has(item(D.chains, i).group));
  const { stays, walks, touching, chainsAt, reach: closure } = net;
  const edges = net.rides.map((r): Edge => {
    const [a, b] = r.pts;
    const info = `${item(D.trips, r.trip)} · ${timeFmt.format(a[0])} ${item(D.names, a[2])} → ${timeFmt.format(b[0])} ${item(D.names, b[2])}`;
    return { ...r, info };
  });
  // Hover targets, looked up by index from a single set of event listeners.
  const targets: HitTarget[] = [];
  for (const e of edges) {
    if (e.pts[0][2] === e.pts[1][2]) continue;
    // On this train between two stops: reachable is what could lead onto it,
    // what it leads to, and the connections riding it.
    const [a, b] = e.pts;
    const id = `ride:${String(e.trip)}:${String(a[0])}:${String(b[0])}`;
    let t: Target | null = null;
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
            segments: closure({ at: [], back: [[a[2], a[0]]], ride: { trip: e.trip, from: a[0], until: a[0] } }),
          },
        };
        return t;
      },
    });
  }
  for (const [s, { y, spans }] of stays) {
    let reach: Reach | null = null;
    for (const [from, to] of runsOf(spans))
      targets.push({
        pts: [
          [from, y, s],
          [to, y, s],
        ],
        at: (time) => {
          const { t, here, pts } = stayAt(spans, from, to, time, y, s);
          const trains = [...new Set(here.flatMap((w) => (w.trip === undefined ? [] : [w.trip])))];
          const waiting = here.some((w) => w.trip === undefined);
          const parts = [waiting ? "waiting" : null, ...trains.map((tr) => `standing: ${item(D.trips, tr)}`)].filter(
            (x): x is string => x !== null,
          );
          // What is reachable only changes when t passes a span end here, so the
          // two counts identify the highlight; it is rebuilt only when they change.
          const key = `${String(s)}:${String(spans.filter((w) => w.from <= t).length)}:${String(spans.filter((w) => w.to >= t).length)}`;
          const r: Reach = reach?.key === key ? reach : { key, segments: closure({ at: [[s, t]] }) };
          reach = r;
          const trip = trains.length === 1 && !waiting ? trains[0] : undefined;
          return {
            id: `stay:${String(s)}:${String(from)}`,
            info: `at ${item(D.names, s)} · ${timeFmt.format(t)} · ${parts.join(" · ")}`,
            ...(trip === undefined ? {} : { trip }),
            chains: chainsAt(s, t),
            reach: r,
            pts,
          };
        },
      });
  }
  for (const [pi, chains] of walks) {
    const p = item(D.pieces, pi);
    const t: Target = { id: `walk:${String(pi)}`, info: p.info, chains: [...chains], pts: p.pts };
    targets.push({ pts: t.pts, at: () => t });
  }
  // A station on the axis: every connection touching it, and what is
  // reachable through it at any time (arriving whenever, leaving whenever).
  const stationTargets = new Map<number, Target>();
  const stationTarget = (s: number): Target =>
    entry(stationTargets, s, () => {
      const chains = touching.get(s) ?? new Set<number>();
      const h = heightOf(s);
      // Only stations with a row have a label to hover.
      if (h === null) throw new Error(`station ${String(s)} has no height`);
      return {
        id: `station:${String(s)}`,
        info: `${item(D.names, s)} · any time`,
        chains: [...chains],
        pts: [
          [full.t0, h, s],
          [full.t1, h, s],
        ],
        reach: {
          key: `station:${String(s)}`,
          with: chains,
          segments: closure({ at: [], fwd: [[s, -Infinity]], back: [[s, Infinity]] }),
        },
      };
    });
  model = {
    edges,
    waits: net.waits,
    walks: [...walks.keys()],
    targets,
    stationTarget,
  };
}

function render(): void {
  const md = model;
  if (!md) return;
  plotEl.querySelectorAll("svg").forEach((s) => {
    s.remove();
  });
  const W = plotEl.clientWidth,
    H = plotEl.clientHeight;
  // Station names take up to 40 % of a narrow plot (phones), else 240 px.
  const m = { l: Math.min(240, Math.round(W * 0.4)), r: 16, t: 14, b: 28 };
  geom = { W, H, m };
  const x = (t: number): number => m.l + ((t - dom.t0) / (dom.t1 - dom.t0)) * (W - m.l - m.r);
  const y = (v: number): number => m.t + ((dom.y1 - v) / (dom.y1 - dom.y0 || 1)) * (H - m.t - m.b);
  const svg = el("svg", { width: W, height: H });
  plotEl.insertBefore(svg, tip);
  const clip = el("clipPath", { id: "area" }, el("defs", {}, svg));
  el("rect", { x: m.l, y: m.t - 6, width: W - m.l - m.r, height: H - m.b - m.t + 6 }, clip);

  // Station bands, alternating hue, so neighbouring stations are told apart.
  // Hue follows the station, not its position, so it stays put when zooming.
  const hue = (i: number): number => (i * 47) % 360;
  const st = rows.map((s) => ({ ...s, py: y(s.y) })).filter((s) => s.py >= m.t - 1 && s.py <= H - m.b + 1);
  const bands = el("g", { "clip-path": "url(#area)" }, svg);
  st.forEach((s, k) => {
    const prev = st[k - 1],
      next = st[k + 1];
    const above = prev ? (prev.py + s.py) / 2 : m.t;
    const below = next ? (s.py + next.py) / 2 : H - m.b;
    el(
      "rect",
      {
        x: m.l,
        y: above,
        width: W - m.l - m.r,
        height: Math.max(0, below - above),
        fill: `hsl(${String(hue(s.i))}, 60%, 50%)`,
        "fill-opacity": 0.05,
      },
      bands,
    );
  });

  // Every visible station gets a label: labels of nearby stations are pushed
  // apart and joined to their row by a leader line in the row's band colour.
  const gap = Math.min(12, (H - m.t - m.b) / Math.max(1, st.length));
  const at = spreadLabels(
    st.map((s) => s.py),
    gap,
    H - m.b,
  );
  const stations = el("g", { class: "station" }, svg);
  st.forEach((s, k) => {
    const ak = item(at, k);
    el("line", { x1: m.l, x2: W - m.r, y1: s.py, y2: s.py }, stations);
    el(
      "polyline",
      {
        points: `${String(m.l - 26)},${String(ak)} ${String(m.l - 8)},${String(s.py)} ${String(m.l)},${String(s.py)}`,
        class: "leader",
        stroke: `hsl(${String(hue(s.i))}, 60%, 50%)`,
      },
      stations,
    );
    const t = el(
      "text",
      { x: m.l - 30, y: ak + gap / 3, "text-anchor": "end", "font-size": Math.min(11, gap), class: "station-name" },
      stations,
    );
    fitText(t, s.name, m.l - 34);
    // Like a line in the plot: hovering previews what passes through the
    // station at any time, clicking lists the connections touching it.
    t.addEventListener("mousemove", (e) => {
      if (drag) return;
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
      if (!dragged) choose(md.stationTarget(s.i));
    });
  });

  // Time ticks: the coarsest step that still gives about ten labels.
  const axis = el("g", { class: "axis" }, svg);
  const step = tickStep(dom.t1 - dom.t0, W - m.l - m.r);
  for (let t = Math.ceil(dom.t0 / step) * step; t <= dom.t1; t += step) {
    el("line", { x1: x(t), x2: x(t), y1: m.t, y2: H - m.b }, axis);
    el("text", { x: x(t), y: H - 10, "text-anchor": "middle" }, axis).textContent = tickFmt.format(t);
  }

  const pts = (p: { pts: readonly XY[] }): string =>
    p.pts.map(([t, v]) => `${x(t).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const base = el("g", { class: "base", "clip-path": "url(#area)" }, svg);
  for (const e of md.edges) el("polyline", { points: pts(e), class: "ride", stroke: trainColor(e.trip) }, base);
  for (const pi of md.waits) el("polyline", { points: pts(item(D.pieces, pi)), class: "wait" }, base);
  for (const pi of md.walks) el("polyline", { points: pts(item(D.pieces, pi)), class: "walk" }, base);
  const overlay = el("g", { class: "overlay", "clip-path": "url(#area)" }, svg);

  const hits = el("g", { "clip-path": "url(#area)" }, svg);
  md.targets.forEach((t, i) => el("polyline", { points: pts(t), class: "hit", "data-t": i }, hits));
  const timeAt = (e: MouseEvent): number =>
    dom.t0 + ((e.clientX - svg.getBoundingClientRect().left - m.l) / (W - m.l - m.r)) * (dom.t1 - dom.t0);
  // Hit lines are 9 px wide, so lines of nearby stations (Ludwigshafen Mitte
  // 2 min above Mannheim Hbf) overlap: the line nearest to the mouse wins, not
  // the one drawn last.
  const nearest = (e: MouseEvent, key: string): number | null => {
    const r = svg.getBoundingClientRect(),
      px = e.clientX - r.left,
      py = e.clientY - r.top;
    let best: number | null = null,
      bestD = Infinity;
    for (const h of document.elementsFromPoint(e.clientX, e.clientY)) {
      if (!(h instanceof SVGPolylineElement)) continue;
      const i = h.dataset[key];
      if (i === undefined) continue;
      const d = distanceToLine(px, py, [...h.points]);
      if (d < bestD) {
        bestD = d;
        best = Number(i);
      }
    }
    return best;
  };
  const targetAt = (e: MouseEvent): Target | null => {
    const i = nearest(e, "t");
    return i === null ? null : item(md.targets, i).at(timeAt(e));
  };
  hits.addEventListener("mousemove", (e) => {
    if (drag) return;
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
    if (dragged) return;
    const t = targetAt(e);
    if (t) choose(t);
  });

  const hoverLayer = el("g", { "clip-path": "url(#area)" }, svg);
  const fbLayer = el("g", { class: "fallback", "clip-path": "url(#area)" }, svg);
  const fbTargetAt = (e: MouseEvent): Hover | null => {
    const i = nearest(e, "f");
    return i === null ? null : (view?.fbTargets[i]?.at(timeAt(e)) ?? null);
  };
  fbLayer.addEventListener("mousemove", (e) => {
    if (drag) return;
    hoverTarget = fbTargetAt(e);
    refresh();
    moveTip(e);
  });
  fbLayer.addEventListener("mouseout", () => {
    hoverTarget = null;
    refresh();
  });
  // Clicking a fallback keeps the selection it belongs to.
  fbLayer.addEventListener("click", (e) => {
    e.stopPropagation();
  });
  const labels = el("g", { class: "change-labels", "clip-path": "url(#area)" }, svg);
  svg.addEventListener("click", () => {
    if (dragged) return;
    clearPicks();
    refresh();
  });

  view = { svg, x, y, pts, overlay, hoverLayer, fbLayer, labels, overlayKey: null, fbKey: undefined, fbTargets: [] };
  refresh();
}

// Zoom and pan: wheel zooms around the mouse (Shift: time only, Alt: height
// only), dragging pans, two fingers pinch (sideways: time, up and down:
// height), double-click or the button resets.
let frame: number | null = null;
function scheduleRender(): void {
  frame ??= requestAnimationFrame(() => {
    frame = null;
    render();
  });
}
function mouseInPlot(e: MouseEvent, { W, H, m }: Geom): { px: number; py: number; fx: number; fy: number } {
  const r = plotEl.getBoundingClientRect();
  const px = e.clientX - r.left,
    py = e.clientY - r.top;
  return { px, py, fx: (px - m.l) / (W - m.l - m.r), fy: (py - m.t) / (H - m.t - m.b) };
}
plotEl.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    if (!geom) return;
    const { fx, fy } = mouseInPlot(e, geom);
    dom = zoom(dom, full, { fx, fy }, Math.exp(e.deltaY * 0.0015), { time: !e.altKey, height: !e.shiftKey });
    scheduleRender();
  },
  { passive: false },
);
let drag: { x: number; y: number; dom: Domain } | null = null,
  dragged = false;
/** Pointers down on the plot (mouse, pen or fingers), where they are now. */
const fingers = new Map<number, Finger>();
let pinching: { from: [Finger, Finger]; dom: Domain } | null = null;
const inPlot = (e: PointerEvent): Finger => {
  const r = plotEl.getBoundingClientRect();
  return { x: e.clientX - r.left, y: e.clientY - r.top };
};
const two = (): [Finger, Finger] | null => {
  const [a, b] = fingers.values();
  return a !== undefined && b !== undefined ? [a, b] : null;
};
plotEl.addEventListener("pointerdown", (e) => {
  if (e.pointerType === "mouse" && e.button !== 0) return;
  fingers.set(e.pointerId, inPlot(e));
  const pair = two();
  if (pair) {
    // A second finger: pinch from here; nothing gets picked when they lift.
    pinching = { from: pair, dom: { ...dom } };
    drag = null;
    dragged = true;
    tip.style.display = "none";
  } else {
    drag = { x: e.clientX, y: e.clientY, dom: { ...dom } };
    dragged = false;
  }
});
window.addEventListener("pointermove", (e) => {
  if (!fingers.has(e.pointerId) || !geom) return;
  fingers.set(e.pointerId, inPlot(e));
  const pair = two();
  if (pinching && pair) {
    dom = pinch(pinching.dom, full, pinching.from, pair, geom);
    scheduleRender();
    return;
  }
  if (!drag) return;
  const dx = e.clientX - drag.x,
    dy = e.clientY - drag.y;
  if (!dragged && Math.hypot(dx, dy) < 4) return;
  dragged = true;
  tip.style.display = "none";
  dom = pan(drag.dom, full, dx, dy, geom);
  scheduleRender();
});
const lift = (e: PointerEvent): void => {
  if (!fingers.delete(e.pointerId)) return;
  pinching = null;
  drag = null;
  // The click that ends a drag must not select or clear; let it pass first.
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

// Clicking a line or station picks it; clicking it again unpicks it. Several
// picks narrow down: the listed connections pass all of them, the highlight is
// what is reachable through each. One connection left is selected directly.
function choose(target: Target | Picked): void {
  picks = toggle(picks, {
    id: target.id,
    info: target.info,
    pts: target.pts,
    chains: target.chains,
    reach: target.reach ?? null,
  });
  applyPicks();
  refresh();
}
function applyPicks(): void {
  candidates = narrow(picks, (i) => !hidden.has(item(D.chains, i).group));
  pinned = candidates === null ? null : pinAfter(candidates.chains, pinned);
}
function clearPicks(): void {
  picks = [];
  candidates = null;
  pinned = null;
  preview = null;
}

function refresh(): void {
  if (!view) return;
  const { svg, x, y, pts, overlay, hoverLayer, fbLayer, labels } = view;
  const strong = [pinned, preview].filter((i): i is number => i !== null);
  // Hovered stretch and hovered group show at full strength; the rest of a
  // clicked list recedes once one of its connections is selected or previewed.
  const soft = new Set<number>(),
    faded = new Set<number>();
  // Hovering shows what is reachable through the point; a clicked point keeps
  // showing it, receding once one of its connections is selected or previewed.
  const reach = hoverTarget?.reach ?? candidates?.reach ?? null;
  const reachFaded = !hoverTarget?.reach && strong.length > 0;
  if (hoverTarget && !hoverTarget.reach) hoverTarget.chains.forEach((i) => soft.add(i));
  if (hoverTarget?.reach?.with) hoverTarget.reach.with.forEach((i) => soft.add(i));
  if (softGroup !== null)
    D.chains.forEach((c, i) => {
      if (c.group === softGroup && !hidden.has(c.group)) soft.add(i);
    });
  if (candidates) candidates.chains.forEach((i) => (strong.length ? faded : soft).add(i));
  svg.classList.toggle("focus", strong.length > 0 || soft.size > 0 || faded.size > 0 || reach !== null);

  // Overlay: previewed connections in train colours, the selection thick on top.
  // Redrawn only when the set changes, not on every mouse move.
  const key = JSON.stringify([
    strong,
    [...soft].sort((a, b) => a - b),
    [...faded].sort((a, b) => a - b),
    reach?.key,
    reachFaded,
  ]);
  if (key !== view.overlayKey) {
    view.overlayKey = key;
    overlay.replaceChildren();
    // A piece, or its stretch between two times (reach highlights).
    const draw = (pi: number, cls: string, from = -Infinity, to = Infinity): void => {
      const p = item(D.pieces, pi);
      const s = clipLine(p.pts, from, to);
      if (s.length < 2 && p.pts.length >= 2) return;
      const line = el(
        "polyline",
        { points: pts({ pts: s }), class: `${p.kind}${p.warn ? " warn" : ""} ${cls}` },
        overlay,
      );
      if (p.kind === "ride" && p.trip !== null) line.setAttribute("stroke", trainColor(p.trip));
    };
    const mine = new Set(strong.flatMap((i) => item(D.chains, i).pieces));
    const shown = new Set(mine);
    const pieces = (chains: Iterable<number>): Set<number> =>
      new Set([...chains].flatMap((i) => item(D.chains, i).pieces).filter((pi) => !shown.has(pi)));
    const bright = pieces(soft);
    bright.forEach((pi) => shown.add(pi));
    if (reach && reachFaded)
      for (const [pi, ...cut] of reach.segments) if (!shown.has(pi)) draw(pi, "soft faded", ...cut);
    for (const pi of pieces(faded)) draw(pi, "soft faded");
    for (const pi of bright) draw(pi, "soft");
    if (reach && !reachFaded) for (const [pi, ...cut] of reach.segments) if (!shown.has(pi)) draw(pi, "soft", ...cut);
    for (const pi of mine) draw(pi, "strong");
  }

  hoverLayer.replaceChildren();
  // The picks stay marked, under what is hovered.
  for (const p of picks) el("polyline", { points: pts(p), class: "picked-piece" }, hoverLayer);
  if (hoverTarget) el("polyline", { points: pts(hoverTarget), class: "hover-piece" }, hoverLayer);

  // Fallbacks of the selection, dashed in train colours, hoverable. Redrawn
  // only when the selection changes, so hovering them doesn't replace them.
  const fbKey = (focus: number | null): number | null => (focus === pinned ? focus : null);
  const focus = preview ?? pinned;
  // Drawn once the selection's details are known.
  const fbDetail = ((k) => (k === null ? undefined : detailOf(k)))(fbKey(focus));
  const fk = fbDetail === undefined ? null : fbKey(focus);
  if (fk !== view.fbKey) {
    view.fbKey = fk;
    fbLayer.replaceChildren();
    const fbTargets: FbTarget[] = [];
    view.fbTargets = fbTargets;
    if (fbDetail !== undefined) {
      // Fallbacks share stretches (the same S1, the wait after the missed
      // train): one target per stretch and per continuous stay, naming all.
      const byPiece = new Map<Piece, Set<Fallback>>(),
        waits = new Map<number, { y: number; spans: { from: number; to: number; f: Fallback }[] }>();
      for (const f of fbDetail.fallbacks) {
        const g = el("g", {}, fbLayer);
        for (const p of f.pieces) {
          const line = el("polyline", { points: pts(p), class: `${p.kind}${p.warn ? " warn" : ""}` }, g);
          if (p.kind === "ride" && p.trip !== null) line.setAttribute("stroke", trainColor(p.trip));
          const first = p.pts[0],
            last = p.pts[p.pts.length - 1];
          if (!first || !last) continue;
          if (p.kind === "wait") {
            const [from, wy, s] = first,
              [to] = last;
            entry(waits, s, () => ({ y: wy, spans: [] })).spans.push({ from, to, f });
          } else {
            entry(byPiece, p, () => new Set()).add(f);
          }
        }
      }
      const hit = (points: Pt[], at: (time: number) => Hover): void => {
        fbTargets.push({ at });
        el("polyline", { points: pts({ pts: points }), class: "hit", "data-f": fbTargets.length - 1 }, fbLayer);
      };
      for (const [p, fs] of byPiece) {
        const t: Hover = {
          info: p.info,
          ...(p.trip === null ? {} : { trip: p.trip }),
          chains: [],
          fallbacks: [...fs],
          pts: p.pts,
        };
        hit(p.pts, () => t);
      }
      for (const [s, { y: wy, spans }] of waits) {
        for (const [from, to] of runsOf(spans))
          hit(
            [
              [from, wy, s],
              [to, wy, s],
            ],
            (time) => {
              const { t, here, pts: stayPts } = stayAt(spans, from, to, time, wy, s);
              return {
                info: `waiting at ${item(D.names, s)} · ${timeFmt.format(t)}`,
                chains: [],
                fallbacks: [...new Set(here.map((w) => w.f))],
                pts: stayPts,
              };
            },
          );
      }
    }
  }
  labels.replaceChildren();
  if (focus !== null) {
    // Name the changes: their station rows are often too close to tell apart.
    for (const p of item(D.chains, focus)
      .pieces.map((i) => item(D.pieces, i))
      .filter((p) => p.kind === "wait")) {
      const first = p.pts[0];
      if (!first) continue;
      const [t, v] = first;
      el("text", { x: x(t) + 4, y: y(v) - 5 }, labels).textContent = p.info;
    }
  }

  if (hoverTarget) {
    const { info, chains, trip } = hoverTarget;
    const swatch = trip === undefined ? "" : `<span style="color:${trainColor(trip)}">■</span> `;
    let who: string;
    const only = chains.length === 1 ? chains[0] : undefined;
    if (hoverTarget.fallbacks) {
      const at = [...new Set(hoverTarget.fallbacks.map((f) => f.at))];
      who =
        `<br><span style="color:var(--accent)">If the change at ${esc(at.join(" / "))} is missed:</span>` +
        hoverTarget.fallbacks
          .map((f) => `<br><span style="color:var(--muted)">• ${esc(f.line)} · arrive ${esc(f.arrive)}</span>`)
          .join("");
    } else if (only !== undefined) {
      want([only]);
      const route = routeOf(only);
      who = `<br><b>${esc(item(D.chains, only).header)}</b><span style="color:var(--muted)">${esc(route)}</span>`;
    } else {
      const groups = new Map<number, number>();
      for (const i of chains) {
        const g = item(D.chains, i).group;
        groups.set(g, (groups.get(g) ?? 0) + 1);
      }
      const list = [...groups]
        .sort((a, b) => b[1] - a[1])
        .map(([g, n]) => `• ${esc(item(D.groups, g).label)} (${String(n)})`)
        .join("<br>");
      who = chains.length
        ? `<br><span style="color:var(--muted)">${String(chains.length)} of the listed connections in ${String(groups.size)} group${groups.size > 1 ? "s" : ""} · click to list them</span><br>${list}`
        : `<br><span style="color:var(--muted)">none of the listed connections · on others within the limits</span>`;
    }
    tip.innerHTML = `<b>${swatch}${esc(info)}</b>${who}`;
    tip.style.display = "block";
  } else {
    tip.style.display = "none";
  }
  renderPanel();
}

function moveTip(e: MouseEvent): void {
  const r = plotEl.getBoundingClientRect();
  tip.style.left = `${String(Math.min(e.clientX - r.left + 14, r.width - tip.offsetWidth - 8))}px`;
  tip.style.top = `${String(e.clientY - r.top + 14)}px`;
}

let panelKey: string | null = null; // avoid rebuilding the panel (and losing its hover) on every mouse move
function renderPanel(): void {
  const key = JSON.stringify([pinned, picks.map((p) => p.id), candidates?.chains.length ?? null]);
  if (key === panelKey) return;
  panelKey = key;
  // The picks, each unpicked by clicking it (or its line again).
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
    let html =
      candidates && candidates.chains.length > 1
        ? `<span class="back" id="back">← all ${String(candidates.chains.length)} connections</span>`
        : "";
    const route = routeOf(pinned);
    html += `<b>${esc(c.header)}</b>\n${esc(route).replaceAll(" ‖", "\n‖").replaceAll("‖ ", "‖\n")}`;
    let at: string | null = null;
    for (const f of d?.fallbacks ?? []) {
      if (f.at !== at) {
        html += `\n\n<span class="fb">If the change at ${esc(f.at)} is missed:</span>`;
        at = f.at;
      }
      html += `\n<span class="fb">• ${esc(f.line)} · arrive ${esc(f.arrive)}</span>`;
    }
    details.innerHTML = html;
    document.getElementById("back")?.addEventListener("click", () => {
      pinned = null;
      refresh();
    });
  } else if (candidates) {
    panelTitle.textContent =
      picks.length > 1
        ? `${String(candidates.chains.length)} connections pass all ${String(picks.length)}`
        : `${String(candidates.chains.length)} listed connections use it`;
    details.replaceChildren();
    // The first ones: the rest takes picks to narrow down, not scrolling.
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
  if (!candidates || (e.key !== "ArrowDown" && e.key !== "ArrowUp")) return;
  e.preventDefault();
  pinned = step(candidates.chains, pinned, e.key === "ArrowDown");
  refresh();
});

const groupsEl = byId("groups", HTMLDivElement);
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
    if (hidden.has(i)) hidden.delete(i);
    else hidden.add(i);
    row.classList.toggle("off", hidden.has(i));
    softGroup = null;
    if (pinned !== null && item(D.chains, pinned).group === i) pinned = null;
    applyPicks();
    panelKey = null;
    buildModel();
    render();
  });
  groupsEl.appendChild(row);
});

buildModel();
new ResizeObserver(render).observe(plotEl);
