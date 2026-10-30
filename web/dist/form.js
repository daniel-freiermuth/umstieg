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

// fields.ts
var kinds = ["text", "number", "flag", "choice", "time", "zone", "station", "stations"];
var decodeField = (v, path) => {
  const o = object(v, path);
  return {
    name: field(o, "name", string, path),
    help: field(o, "help", string, path),
    kind: field(o, "kind", oneOf(...kinds), path),
    default: field(o, "default", string, path),
    choices: field(o, "choices", array(string), path),
    advanced: field(o, "advanced", boolean, path)
  };
};
var decodeSpec = (v, path) => {
  const o = object(v, path);
  return {
    fields: field(o, "fields", array(decodeField), path),
    values: field(o, "values", record(string), path),
    error: field(o, "error", nullable(string), path)
  };
};
var label = (name) => name.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
var given = (values, name) => Object.hasOwn(values, name) ? values[name] : undefined;
function wallTime(d, zone) {
  const format = new Intl.DateTimeFormat("sv-SE", {
    timeZone: zone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23"
  });
  return format.format(d).replace(" ", "T");
}
var zoneOf = (values, userZone) => given(values, "time_zone") ?? userZone;
function initial(f, values, now, userZone) {
  const value = given(values, f.name);
  if (f.kind === "flag")
    return value !== undefined;
  if (f.kind === "zone")
    return zoneOf(values, userZone);
  if (f.kind === "time")
    return value ?? wallTime(now, zoneOf(values, userZone));
  return value ?? f.default;
}
var TRIP = ["from", "to", "at"];
var PLACED = [...TRIP, "time_zone", "via"];
function sections(fields) {
  const first = (name) => fields.find((f) => f.name === name);
  const trip = TRIP.flatMap((name) => fields.filter((f) => f.name === name).slice(0, 1));
  const rest = fields.filter((f) => !PLACED.includes(f.name));
  return {
    trip,
    zone: first("time_zone"),
    via: first("via"),
    options: rest.filter((f) => !f.advanced),
    advanced: rest.filter((f) => f.advanced)
  };
}
var changed = (fields, values) => fields.filter((f) => {
  const value = given(values, f.name);
  return value !== undefined && value !== "" && value !== f.default;
}).length;
var omitted = (f, value) => f.kind !== "flag" && f.kind !== "station" && value === f.default;
var decodeStop = (v, path) => {
  const o = object(v, path);
  return { name: field(o, "name", string, path), id: field(o, "id", string, path) };
};
var decodeStops = array(decodeStop);
function stationSearch(text) {
  const q = text.trim();
  return q.length < 3 ? null : q;
}
var split = (list) => list.split(";").map((s) => s.trim()).filter(Boolean);
function picked(values, name) {
  const names = split(given(values, name) ?? "");
  const ids = split(given(values, `${name}_id`) ?? "");
  if (names.length !== ids.length)
    return [];
  return names.map((n, i) => ({ name: n, id: item(ids, i) }));
}
function unpicked(values, name) {
  return picked(values, name).length > 0 ? [] : split(given(values, name) ?? "");
}

// form.ts
var SPEC = decodeSpec(parseJson(byId("spec", HTMLScriptElement).text), "spec");
var form = byId("form", HTMLFormElement);
if (SPEC.error !== null)
  Object.assign(byId("error", HTMLDivElement), { textContent: SPEC.error, hidden: false });
var NOT_PICKED = "Choose a station from the suggestions";
var element = (tag, props = {}) => Object.assign(document.createElement(tag), props);
var known = new Map;
async function lookup(q) {
  const response = await fetch(`/stops?q=${encodeURIComponent(q)}`);
  if (!response.ok)
    return [];
  const stops = decodeStops(parseJson(await response.text()), "stops");
  for (const s of stops)
    if (!known.has(s.name))
      known.set(s.name, s.id);
  return stops;
}
var pickedNow = (text) => {
  const id = known.get(text);
  return id === undefined ? null : { name: text, id };
};
var lists = 0;
function stationText(id, typed) {
  const input = element("input", { type: "text", id, autocomplete: "off" });
  const list = element("datalist", { id: `stops-${String(lists++)}` });
  input.setAttribute("list", list.id);
  document.body.append(list);
  let timer;
  input.addEventListener("input", () => {
    typed(pickedNow(input.value));
    clearTimeout(timer);
    const q = stationSearch(input.value);
    if (q === null || pickedNow(input.value) !== null)
      return;
    timer = setTimeout(() => {
      lookup(q).then((stops) => {
        list.replaceChildren(...stops.map((s) => new Option(s.name)));
        typed(pickedNow(input.value));
      });
    }, 250);
  });
  return input;
}
var hidden = (name) => element("input", { type: "hidden", name });
function stationField(f, id) {
  const box = element("div", { className: "station" });
  const stationId = hidden(`${f.name}_id`);
  const status = element("div", { className: "status" });
  const show = (stop) => {
    stationId.value = stop?.id ?? "";
    input.classList.toggle("picked", stop !== null);
    const blank = input.value.trim() === "";
    input.setCustomValidity(stop !== null || blank ? "" : NOT_PICKED);
    status.textContent = stop !== null || blank ? "" : NOT_PICKED;
  };
  const input = stationText(id, show);
  Object.assign(input, { name: f.name, required: true, placeholder: "Station" });
  box.append(input, stationId, status);
  const [given2] = picked(SPEC.values, f.name);
  if (given2 !== undefined)
    known.set(given2.name, given2.id);
  input.value = given2?.name ?? unpicked(SPEC.values, f.name).join("; ");
  show(given2 ?? null);
  const q = given2 === undefined ? stationSearch(input.value) : null;
  if (q !== null)
    lookup(q).then(() => {
      show(pickedNow(input.value));
    });
  return {
    box,
    input,
    refresh: () => {
      show(pickedNow(input.value));
    }
  };
}
function viaField(f, id) {
  const box = element("div", { className: "station" });
  const chips = element("div", { className: "chips" });
  const names = hidden(f.name);
  const ids = hidden(`${f.name}_id`);
  const stops = [...picked(SPEC.values, f.name)];
  const render = () => {
    names.value = stops.map((s) => s.name).join(";");
    ids.value = stops.map((s) => s.id).join(";");
    chips.replaceChildren(...stops.map((s, i) => {
      const chip = element("span", { className: "chip", textContent: s.name });
      const remove = element("button", { type: "button", textContent: "×", title: `Remove ${s.name}` });
      remove.addEventListener("click", () => {
        stops.splice(i, 1);
        render();
      });
      chip.append(remove);
      return chip;
    }));
  };
  const check = () => {
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
  const listed = unpicked(SPEC.values, f.name);
  const text = listed.join("; ");
  input.value = text;
  check();
  const lookups = listed.map((name) => {
    const q = stationSearch(name);
    return q === null ? Promise.resolve([]) : lookup(q);
  });
  if (listed.length > 0)
    Promise.all(lookups).then(() => {
      stops.unshift(...listed.map(pickedNow).filter((s) => s !== null));
      render();
      if (input.value === text)
        input.value = listed.filter((name) => pickedNow(name) === null).join("; ");
      check();
    });
  return box;
}
var USER_ZONE = Intl.DateTimeFormat().resolvedOptions().timeZone;
function row(f, control, id, help = true) {
  const box = element("div", { className: `field ${f.kind}` });
  const l = element("label", { htmlFor: id, textContent: f.kind === "time" ? "Departure" : label(f.name) });
  if (f.kind === "flag")
    box.append(control, l);
  else if (f.kind === "stations")
    box.append(control);
  else
    box.append(l, control);
  if (help && f.help !== "")
    box.append(element("div", { className: "help", textContent: f.help }));
  return box;
}
function control(f, id) {
  const value = initial(f, SPEC.values, new Date, USER_ZONE);
  if (typeof value === "boolean")
    return element("input", { type: "checkbox", checked: value, id, name: f.name });
  if (f.kind === "choice") {
    const select = element("select", { id, name: f.name });
    for (const c of f.choices)
      select.append(new Option(c, c, false, c === value));
    return select;
  }
  const text = element("input", { id, name: f.name, value });
  text.type = f.kind === "number" ? "number" : f.kind === "time" ? "datetime-local" : "text";
  if (f.kind === "number")
    text.step = "any";
  return text;
}
var optionRow = (f) => row(f, control(f, `f-${f.name}`), `f-${f.name}`);
function fold(title, count, className) {
  const details = element("details", { className: `fold ${className}`, open: count > 0 });
  const summary = element("summary", { textContent: title });
  if (count > 0)
    summary.append(element("span", { className: "badge", textContent: String(count) }));
  details.append(summary);
  return details;
}
function timeRow(f, zoneField) {
  const box = row(f, control(f, "f-at"), "f-at", false);
  if (zoneField === undefined)
    return box;
  const zone = element("input", {
    id: "f-time_zone",
    name: zoneField.name,
    value: zoneOf(SPEC.values, USER_ZONE),
    required: true
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
var placed = sections(SPEC.fields);
var [fromField, toField, atField] = placed.trip;
var trip = element("div", { className: "trip" });
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
if (atField !== undefined)
  trip.append(timeRow(atField, placed.zone));
var more = element("div", { className: "more" });
if (placed.via !== undefined) {
  const via = fold("Via", picked(SPEC.values, "via").length + unpicked(SPEC.values, "via").length, "via");
  via.append(row(placed.via, viaField(placed.via, "f-via"), "f-via"));
  more.append(via);
}
var advanced = fold("Advanced", changed(placed.advanced, SPEC.values), "advanced");
advanced.append(...placed.advanced.map(optionRow));
var options = fold("Options", changed([...placed.options, ...placed.advanced], SPEC.values), "options");
options.append(...placed.options.map(optionRow), advanced);
more.append(options);
form.append(trip, element("button", { type: "submit", className: "go", textContent: "Search" }), more);
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
      if (input instanceof HTMLInputElement || input instanceof HTMLSelectElement)
        input.disabled = false;
    }
  });
});
