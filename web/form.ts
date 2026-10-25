// Search form of `umstieg serve`, built from the field descriptions the server
// derives from the command-line options.

import { type Decoder, array, boolean, field, nullable, object, oneOf, parseJson, record, string } from "./decode";
import { byId } from "./dom";

const kinds = ["text", "number", "flag", "choice", "time", "station", "stations"] as const;
type Kind = (typeof kinds)[number];

interface Field {
  name: string;
  help: string;
  kind: Kind;
  default: string;
  choices: string[];
  advanced: boolean;
}

interface Spec {
  fields: Field[];
  /** The form's values from the URL (a repeated or changed search). */
  values: Record<string, string>;
  error: string | null;
}

const decodeField: Decoder<Field> = (v, path) => {
  const o = object(v, path);
  return {
    name: field(o, "name", string, path),
    help: field(o, "help", string, path),
    kind: field(o, "kind", oneOf(...kinds), path),
    default: field(o, "default", string, path),
    choices: field(o, "choices", array(string), path),
    advanced: field(o, "advanced", boolean, path),
  };
};

const decodeSpec: Decoder<Spec> = (v, path) => {
  const o = object(v, path);
  return {
    fields: field(o, "fields", array(decodeField), path),
    values: field(o, "values", record(string), path),
    error: field(o, "error", nullable(string), path),
  };
};

const SPEC = decodeSpec(parseJson(byId("spec", HTMLScriptElement).text), "spec");
const form = byId("form", HTMLFormElement);
if (SPEC.error !== null) Object.assign(byId("error", HTMLDivElement), { textContent: SPEC.error, hidden: false });

// Where fields go: the trip first, then limits, the rest folded away.
const TRIP = ["from", "to", "at", "window", "via"];
const label = (name: string): string => name.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
const pad = (n: number): string => String(n).padStart(2, "0");
const now = (): string => {
  const d = new Date();
  return `${String(d.getFullYear())}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
};
const given = (name: string): string | undefined => (Object.hasOwn(SPEC.values, name) ? SPEC.values[name] : undefined);

function fieldRow(f: Field): HTMLDivElement {
  const row = document.createElement("div");
  row.className = "field";
  const id = `f-${f.name}`;
  const value = given(f.name) ?? f.default;
  let input: HTMLInputElement | HTMLSelectElement;
  if (f.kind === "flag") {
    input = Object.assign(document.createElement("input"), { type: "checkbox", checked: given(f.name) !== undefined });
  } else if (f.kind === "choice") {
    input = document.createElement("select");
    for (const c of f.choices) input.append(new Option(c, c, false, c === value));
  } else {
    const text = document.createElement("input");
    text.type = f.kind === "number" ? "number" : f.kind === "time" ? "datetime-local" : "text";
    if (f.kind === "number") text.step = "any";
    text.value = f.kind === "time" && given(f.name) === undefined ? now() : value;
    if (f.kind === "station" || f.kind === "stations") stationInput(text, f.kind === "stations");
    if (f.kind === "station") text.required = true;
    if (f.kind === "stations") text.placeholder = "Several separated by ;";
    input = text;
  }
  Object.assign(input, { id, name: f.name });
  const l = Object.assign(document.createElement("label"), { htmlFor: id, textContent: label(f.name) });
  const help = Object.assign(document.createElement("div"), { className: "help", textContent: f.help });
  row.append(l, input, help);
  return row;
}

// Station names complete from Transitous, the part after the last ";" for lists.
let lists = 0;
function stationInput(input: HTMLInputElement, several: boolean): void {
  const list = Object.assign(document.createElement("datalist"), { id: `stops-${String(lists++)}` });
  input.setAttribute("list", list.id);
  input.autocomplete = "off";
  document.body.append(list);
  let timer: number | undefined;
  input.addEventListener("input", () => {
    clearTimeout(timer);
    const parts = input.value.split(";");
    const q = (several ? (parts.at(-1) ?? "") : input.value).trim();
    const prefix = several
      ? parts
          .slice(0, -1)
          .map((s) => s.trim())
          .filter(Boolean)
          .map((s) => `${s}; `)
          .join("")
      : "";
    if (q.length < 3) return;
    timer = setTimeout(() => {
      void (async () => {
        const response = await fetch(`/stops?q=${encodeURIComponent(q)}`);
        if (!response.ok) return;
        const names = array(string)(parseJson(await response.text()), "stops");
        list.replaceChildren(...names.map((n) => new Option(prefix + n)));
      })();
    }, 250);
  });
}

const trip = document.createElement("fieldset");
trip.append(Object.assign(document.createElement("legend"), { textContent: "Trip" }));
const limits = document.createElement("fieldset");
limits.append(Object.assign(document.createElement("legend"), { textContent: "Limits and display" }));
const advanced = document.createElement("details");
advanced.append(Object.assign(document.createElement("summary"), { textContent: "Advanced" }));
for (const name of TRIP) {
  const f = SPEC.fields.find((f) => f.name === name);
  if (f) trip.append(fieldRow(f));
}
for (const f of SPEC.fields) {
  if (TRIP.includes(f.name)) continue;
  (f.advanced ? advanced : limits).append(fieldRow(f));
}
const submit = Object.assign(document.createElement("button"), { type: "submit", textContent: "Search" });
const note = Object.assign(document.createElement("p"), {
  className: "note",
  textContent: "A search takes from 20 seconds to a few minutes; repeating one within 10 minutes is fast (cached).",
});
form.append(trip, limits, advanced, submit, note);
// Advanced settings that differ from the defaults stay visible.
if (SPEC.fields.some((f) => f.advanced && given(f.name) !== undefined && given(f.name) !== f.default)) {
  advanced.open = true;
}
// Only send what differs from the defaults: shorter links, same search.
form.addEventListener("submit", () => {
  for (const f of SPEC.fields) {
    const element = form.elements.namedItem(f.name);
    const fixed = f.kind === "flag" || f.kind === "station";
    if (!fixed && (element instanceof HTMLInputElement || element instanceof HTMLSelectElement)) {
      if (element.value === f.default) element.disabled = true;
    }
  }
  setTimeout(() => {
    for (const element of form.elements) {
      if (element instanceof HTMLInputElement || element instanceof HTMLSelectElement) element.disabled = false;
    }
  });
});
