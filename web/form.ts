// Search form of `umstieg serve`, built from the field descriptions the server
// derives from the command-line options.

import { array, parseJson, string } from "./decode";
import { byId } from "./dom";
import { type Field, advancedOpen, decodeSpec, initial, label, omitted, sections, stationQuery } from "./fields";

const SPEC = decodeSpec(parseJson(byId("spec", HTMLScriptElement).text), "spec");
const form = byId("form", HTMLFormElement);
if (SPEC.error !== null) Object.assign(byId("error", HTMLDivElement), { textContent: SPEC.error, hidden: false });

function fieldRow(f: Field): HTMLDivElement {
  const row = document.createElement("div");
  row.className = "field";
  const id = `f-${f.name}`;
  const value = initial(f, SPEC.values, new Date());
  let input: HTMLInputElement | HTMLSelectElement;
  if (typeof value === "boolean") {
    input = Object.assign(document.createElement("input"), { type: "checkbox", checked: value });
  } else if (f.kind === "choice") {
    input = document.createElement("select");
    for (const c of f.choices) input.append(new Option(c, c, false, c === value));
  } else {
    const text = document.createElement("input");
    text.type = f.kind === "number" ? "number" : f.kind === "time" ? "datetime-local" : "text";
    if (f.kind === "number") text.step = "any";
    text.value = value;
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
    const query = stationQuery(input.value, several);
    if (query === null) return;
    timer = setTimeout(() => {
      void (async () => {
        const response = await fetch(`/stops?q=${encodeURIComponent(query.q)}`);
        if (!response.ok) return;
        const names = array(string)(parseJson(await response.text()), "stops");
        list.replaceChildren(...names.map((n) => new Option(query.prefix + n)));
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
const placed = sections(SPEC.fields);
trip.append(...placed.trip.map(fieldRow));
limits.append(...placed.limits.map(fieldRow));
advanced.append(...placed.advanced.map(fieldRow));
const submit = Object.assign(document.createElement("button"), { type: "submit", textContent: "Search" });
const note = Object.assign(document.createElement("p"), {
  className: "note",
  textContent: "A search takes from 20 seconds to a few minutes; repeating one within 10 minutes is fast (cached).",
});
form.append(trip, limits, advanced, submit, note);
advanced.open = advancedOpen(SPEC.fields, SPEC.values);
// Only send what differs from the defaults: shorter links, same search.
form.addEventListener("submit", () => {
  for (const f of SPEC.fields) {
    const element = form.elements.namedItem(f.name);
    if ((element instanceof HTMLInputElement || element instanceof HTMLSelectElement) && omitted(f, element.value)) {
      element.disabled = true;
    }
  }
  setTimeout(() => {
    for (const element of form.elements) {
      if (element instanceof HTMLInputElement || element instanceof HTMLSelectElement) element.disabled = false;
    }
  });
});
