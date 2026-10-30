// Search form of `umstieg serve`, built from the field descriptions the server
// derives from the command-line options: the trip up front, the rest folded.

import { parseJson } from "./decode";
import { byId } from "./dom";
import {
  type Field,
  type Stop,
  changed,
  decodeSpec,
  decodeStops,
  initial,
  label,
  omitted,
  picked,
  sections,
  stationSearch,
  unpicked,
  zoneOf,
} from "./fields";

const SPEC = decodeSpec(parseJson(byId("spec", HTMLScriptElement).text), "spec");
const form = byId("form", HTMLFormElement);
if (SPEC.error !== null) Object.assign(byId("error", HTMLDivElement), { textContent: SPEC.error, hidden: false });

const NOT_PICKED = "Choose a station from the suggestions";

const element = <K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> = {},
): HTMLElementTagNameMap[K] => Object.assign(document.createElement(tag), props);

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
  const input = element("input", { type: "text", id, autocomplete: "off" });
  const list = element("datalist", { id: `stops-${String(lists++)}` });
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

const hidden = (name: string): HTMLInputElement => element("input", { type: "hidden", name });

interface StationField {
  box: HTMLElement;
  input: HTMLInputElement;
  /** Shows whether the text names a picked station, after it was set from outside. */
  refresh: () => void;
}

/** Origin or destination: required, and only a picked station counts. */
function stationField(f: Field, id: string): StationField {
  const box = element("div", { className: "station" });
  const stationId = hidden(`${f.name}_id`);
  const status = element("div", { className: "status" });
  const show = (stop: Stop | null): void => {
    stationId.value = stop?.id ?? "";
    input.classList.toggle("picked", stop !== null);
    const blank = input.value.trim() === "";
    input.setCustomValidity(stop !== null || blank ? "" : NOT_PICKED);
    status.textContent = stop !== null || blank ? "" : NOT_PICKED;
  };
  const input = stationText(id, show);
  Object.assign(input, { name: f.name, required: true, placeholder: "Station" });
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
  return {
    box,
    input,
    refresh: () => {
      show(pickedNow(input.value));
    },
  };
}

/** Stations to route through: each picked one becomes a removable chip. */
function viaField(f: Field, id: string): HTMLElement {
  const box = element("div", { className: "station" });
  const chips = element("div", { className: "chips" });
  const names = hidden(f.name);
  const ids = hidden(`${f.name}_id`);
  const stops: Stop[] = [...picked(SPEC.values, f.name)];
  const render = (): void => {
    names.value = stops.map((s) => s.name).join(";");
    ids.value = stops.map((s) => s.id).join(";");
    chips.replaceChildren(
      ...stops.map((s, i) => {
        const chip = element("span", { className: "chip", textContent: s.name });
        const remove = element("button", { type: "button", textContent: "×", title: `Remove ${s.name}` });
        remove.addEventListener("click", () => {
          stops.splice(i, 1);
          render();
        });
        chip.append(remove);
        return chip;
      }),
    );
  };
  // Text that names no station would be lost: it must be picked or cleared.
  const check = (): void => {
    input.setCustomValidity(input.value.trim() === "" ? "" : `${NOT_PICKED}, or clear the field`);
  };
  const input = stationText(id, (stop) => {
    if (stop !== null) {
      stops.push(stop);
      input.value = "";
      render();
    }
    check();
  });
  input.placeholder = "Add a station";
  box.append(chips, input, names, ids);
  render();
  // Names without their ids (an older link): picked, in their order, once every
  // lookup is back, if Transitous suggests exactly them; the others stay as text,
  // to pick or clear.
  const listed = unpicked(SPEC.values, f.name);
  const text = listed.join("; ");
  input.value = text;
  check();
  const lookups = listed.map((name) => {
    const q = stationSearch(name);
    return q === null ? Promise.resolve([]) : lookup(q);
  });
  if (listed.length > 0)
    void Promise.all(lookups).then(() => {
      // Before any the user added meanwhile: the link's come first.
      stops.unshift(...listed.map(pickedNow).filter((s) => s !== null));
      render();
      if (input.value === text) input.value = listed.filter((name) => pickedNow(name) === null).join("; ");
      check();
    });
  return box;
}

/** The user's time zone, as the browser knows it: "Europe/Berlin". */
const USER_ZONE = Intl.DateTimeFormat().resolvedOptions().timeZone;

/** A row: the label (unless the fold says it), the control, and the option's help below if `help`. */
function row(f: Field, control: HTMLElement, id: string, help = true): HTMLDivElement {
  const box = element("div", { className: `field ${f.kind}` });
  const l = element("label", { htmlFor: id, textContent: f.kind === "time" ? "Departure" : label(f.name) });
  if (f.kind === "flag") box.append(control, l);
  else if (f.kind === "stations") box.append(control);
  else box.append(l, control);
  if (help && f.help !== "") box.append(element("div", { className: "help", textContent: f.help }));
  return box;
}

/** An option's input, filled with its initial value. */
function control(f: Field, id: string): HTMLElement {
  const value = initial(f, SPEC.values, new Date(), USER_ZONE);
  if (typeof value === "boolean") return element("input", { type: "checkbox", checked: value, id, name: f.name });
  if (f.kind === "choice") {
    const select = element("select", { id, name: f.name });
    for (const c of f.choices) select.append(new Option(c, c, false, c === value));
    return select;
  }
  const text = element("input", { id, name: f.name, value });
  text.type = f.kind === "number" ? "number" : f.kind === "time" ? "datetime-local" : "text";
  if (f.kind === "number") text.step = "any";
  return text;
}

const optionRow = (f: Field): HTMLDivElement => row(f, control(f, `f-${f.name}`), `f-${f.name}`);

/** A fold; `count` values changed inside open it and show in its title. */
function fold(title: string, count: number, className: string): HTMLDetailsElement {
  const details = element("details", { className: `fold ${className}`, open: count > 0 });
  const summary = element("summary", { textContent: title });
  if (count > 0) summary.append(element("span", { className: "badge", textContent: String(count) }));
  details.append(summary);
  return details;
}

/** The time, its zone below it: a fold to change it, open if not the user's own. */
function timeRow(f: Field, zoneField: Field | undefined): HTMLDivElement {
  const box = row(f, control(f, "f-at"), "f-at", false);
  if (zoneField === undefined) return box;
  const zone = element("input", {
    id: "f-time_zone",
    name: zoneField.name,
    value: zoneOf(SPEC.values, USER_ZONE),
    required: true,
  });
  const list = element("datalist", { id: "zones" });
  list.append(...Intl.supportedValuesOf("timeZone").map((z) => new Option(z)));
  document.body.append(list);
  zone.setAttribute("list", list.id);
  const shown = element("span", { className: "zone-name", textContent: zone.value });
  zone.addEventListener("input", () => {
    shown.textContent = zone.value.trim();
  });
  const summary = element("summary");
  summary.append("Times in ", shown);
  const details = element("details", { className: "zone", open: zone.value !== USER_ZONE });
  details.append(summary, zone, element("div", { className: "help", textContent: zoneField.help }));
  box.append(details);
  return box;
}

const placed = sections(SPEC.fields);
const [fromField, toField, atField] = placed.trip;
const trip = element("div", { className: "trip" });
if (fromField !== undefined && toField !== undefined) {
  const from = stationField(fromField, "f-from");
  const to = stationField(toField, "f-to");
  const swap = element("button", { type: "button", className: "swap", textContent: "⇅", title: "Swap" });
  swap.addEventListener("click", () => {
    [from.input.value, to.input.value] = [to.input.value, from.input.value];
    from.refresh();
    to.refresh();
  });
  trip.append(row(fromField, from.box, "f-from", false), swap, row(toField, to.box, "f-to", false));
}
if (atField !== undefined) trip.append(timeRow(atField, placed.zone));

const more = element("div", { className: "more" });
if (placed.via !== undefined) {
  const via = fold("Via", picked(SPEC.values, "via").length + unpicked(SPEC.values, "via").length, "via");
  via.append(row(placed.via, viaField(placed.via, "f-via"), "f-via"));
  more.append(via);
}
const advanced = fold("Advanced", changed(placed.advanced, SPEC.values), "advanced");
advanced.append(...placed.advanced.map(optionRow));
const options = fold("Options", changed([...placed.options, ...placed.advanced], SPEC.values), "options");
options.append(...placed.options.map(optionRow), advanced);
more.append(options);
form.append(trip, element("button", { type: "submit", className: "go", textContent: "Search" }), more);

// Only send what differs from the defaults: shorter links, same search.
form.addEventListener("submit", () => {
  for (const f of SPEC.fields) {
    const input = form.elements.namedItem(f.name);
    if ((input instanceof HTMLInputElement || input instanceof HTMLSelectElement) && omitted(f, input.value)) {
      input.disabled = true;
    }
  }
  for (const input of form.elements) {
    if (input instanceof HTMLInputElement && input.name.endsWith("_id") && input.value === "") {
      input.disabled = true;
    }
  }
  setTimeout(() => {
    for (const input of form.elements) {
      if (input instanceof HTMLInputElement || input instanceof HTMLSelectElement) input.disabled = false;
    }
  });
});
