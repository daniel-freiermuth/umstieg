// Search form of `umstieg serve`, built from the field descriptions the server
// derives from the command-line options.

import { parseJson } from "./decode";
import { byId } from "./dom";
import {
  type Field,
  type Stop,
  advancedOpen,
  decodeSpec,
  decodeStops,
  initial,
  label,
  omitted,
  picked,
  sections,
  stationSearch,
  unpicked,
} from "./fields";

const SPEC = decodeSpec(parseJson(byId("spec", HTMLScriptElement).text), "spec");
const form = byId("form", HTMLFormElement);
if (SPEC.error !== null) Object.assign(byId("error", HTMLDivElement), { textContent: SPEC.error, hidden: false });

const NOT_PICKED = "Choose a station from the suggestions";

/** Every station suggested so far, by name: typing or choosing a name exactly picks it. */
const known = new Map<string, string>();

async function lookup(q: string): Promise<Stop[]> {
  const response = await fetch(`/stops?q=${encodeURIComponent(q)}`);
  if (!response.ok) return [];
  const stops = decodeStops(parseJson(await response.text()), "stops");
  for (const s of stops) if (!known.has(s.name)) known.set(s.name, s.id);
  return stops;
}

const pickedNow = (text: string): Stop | null => {
  const id = known.get(text);
  return id === undefined ? null : { name: text, id };
};

/**
 * A text input suggesting stations from Transitous as one types; `typed`
 * runs on every change with the station the text names exactly, if any.
 */
let lists = 0;
function stationText(id: string, typed: (stop: Stop | null) => void): HTMLInputElement {
  const input = Object.assign(document.createElement("input"), { type: "text", id, autocomplete: "off" });
  const list = Object.assign(document.createElement("datalist"), { id: `stops-${String(lists++)}` });
  input.setAttribute("list", list.id);
  document.body.append(list);
  let timer: number | undefined;
  input.addEventListener("input", () => {
    typed(pickedNow(input.value));
    clearTimeout(timer);
    const q = stationSearch(input.value);
    if (q === null || pickedNow(input.value) !== null) return;
    timer = setTimeout(() => {
      void lookup(q).then((stops) => {
        list.replaceChildren(...stops.map((s) => new Option(s.name)));
        // The text may name a suggestion exactly by now.
        typed(pickedNow(input.value));
      });
    }, 250);
  });
  return input;
}

const hidden = (name: string): HTMLInputElement =>
  Object.assign(document.createElement("input"), { type: "hidden", name });

/** Origin or destination: required, and only a picked station counts. */
function stationField(f: Field, id: string): HTMLElement {
  const box = Object.assign(document.createElement("div"), { className: "station" });
  const stationId = hidden(`${f.name}_id`);
  const status = Object.assign(document.createElement("div"), { className: "status" });
  const show = (stop: Stop | null): void => {
    stationId.value = stop?.id ?? "";
    input.classList.toggle("picked", stop !== null);
    const blank = input.value.trim() === "";
    input.setCustomValidity(stop !== null || blank ? "" : NOT_PICKED);
    status.textContent = stop !== null ? "✓ found" : blank ? "" : NOT_PICKED;
    status.classList.toggle("todo", stop === null && !blank);
  };
  const input = stationText(id, show);
  Object.assign(input, { name: f.name, required: true });
  box.append(input, stationId, status);
  const [given] = picked(SPEC.values, f.name);
  if (given !== undefined) known.set(given.name, given.id);
  input.value = given?.name ?? unpicked(SPEC.values, f.name).join("; ");
  show(given ?? null);
  // A name without its id (an older link): picked if Transitous suggests exactly it.
  const q = given === undefined ? stationSearch(input.value) : null;
  if (q !== null)
    void lookup(q).then(() => {
      show(pickedNow(input.value));
    });
  return box;
}

/** Stations to route through: each picked one becomes a removable chip. */
function viaField(f: Field, id: string): HTMLElement {
  const box = Object.assign(document.createElement("div"), { className: "station" });
  const chips = Object.assign(document.createElement("div"), { className: "chips" });
  const names = hidden(f.name);
  const ids = hidden(`${f.name}_id`);
  const stops: Stop[] = [...picked(SPEC.values, f.name)];
  const render = (): void => {
    names.value = stops.map((s) => s.name).join(";");
    ids.value = stops.map((s) => s.id).join(";");
    chips.replaceChildren(
      ...stops.map((s, i) => {
        const chip = Object.assign(document.createElement("span"), { className: "chip", textContent: s.name });
        const remove = Object.assign(document.createElement("button"), {
          type: "button",
          textContent: "×",
          title: `Remove ${s.name}`,
        });
        remove.addEventListener("click", () => {
          stops.splice(i, 1);
          render();
        });
        chip.append(remove);
        return chip;
      }),
    );
  };
  const input = stationText(id, (stop) => {
    if (stop !== null) {
      stops.push(stop);
      input.value = "";
      render();
    }
    // Text that names no station would be lost: it must be picked or cleared.
    input.setCustomValidity(input.value.trim() === "" ? "" : `${NOT_PICKED}, or clear the field`);
  });
  input.placeholder = "Add a station";
  box.append(chips, input, names, ids);
  render();
  for (const name of unpicked(SPEC.values, f.name)) {
    const q = stationSearch(name);
    if (q !== null)
      void lookup(q).then(() => {
        const stop = pickedNow(name);
        if (stop !== null) {
          stops.push(stop);
          render();
        }
      });
  }
  return box;
}

function fieldRow(f: Field): HTMLDivElement {
  const row = document.createElement("div");
  row.className = "field";
  const id = `f-${f.name}`;
  const value = initial(f, SPEC.values, new Date());
  let control: HTMLElement;
  if (f.kind === "station") {
    control = stationField(f, id);
  } else if (f.kind === "stations") {
    control = viaField(f, id);
  } else if (typeof value === "boolean") {
    control = Object.assign(document.createElement("input"), { type: "checkbox", checked: value, id, name: f.name });
  } else if (f.kind === "choice") {
    const select = Object.assign(document.createElement("select"), { id, name: f.name });
    for (const c of f.choices) select.append(new Option(c, c, false, c === value));
    control = select;
  } else {
    const text = Object.assign(document.createElement("input"), { id, name: f.name, value });
    text.type = f.kind === "number" ? "number" : f.kind === "time" ? "datetime-local" : "text";
    if (f.kind === "number") text.step = "any";
    control = text;
  }
  const l = Object.assign(document.createElement("label"), { htmlFor: id, textContent: label(f.name) });
  const help = Object.assign(document.createElement("div"), { className: "help", textContent: f.help });
  row.append(l, control, help);
  return row;
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
  for (const element of form.elements) {
    if (element instanceof HTMLInputElement && element.name.endsWith("_id") && element.value === "") {
      element.disabled = true;
    }
  }
  setTimeout(() => {
    for (const element of form.elements) {
      if (element instanceof HTMLInputElement || element instanceof HTMLSelectElement) element.disabled = false;
    }
  });
});
