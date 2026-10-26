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
var kinds = ["text", "number", "flag", "choice", "time", "station", "stations"];
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
var pad = (n) => String(n).padStart(2, "0");
function localDateTime(d) {
  return `${String(d.getFullYear())}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
function initial(f, values, now) {
  const value = given(values, f.name);
  if (f.kind === "flag")
    return value !== undefined;
  if (f.kind === "time")
    return value ?? localDateTime(now);
  return value ?? f.default;
}
var TRIP = ["from", "to", "at", "window", "via"];
function sections(fields) {
  const trip = TRIP.flatMap((name) => fields.filter((f) => f.name === name).slice(0, 1));
  const rest = fields.filter((f) => !TRIP.includes(f.name));
  return { trip, limits: rest.filter((f) => !f.advanced), advanced: rest.filter((f) => f.advanced) };
}
var advancedOpen = (fields, values) => fields.some((f) => f.advanced && given(values, f.name) !== undefined && given(values, f.name) !== f.default);
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
  const input = Object.assign(document.createElement("input"), { type: "text", id, autocomplete: "off" });
  const list = Object.assign(document.createElement("datalist"), { id: `stops-${String(lists++)}` });
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
var hidden = (name) => Object.assign(document.createElement("input"), { type: "hidden", name });
function stationField(f, id) {
  const box = Object.assign(document.createElement("div"), { className: "station" });
  const stationId = hidden(`${f.name}_id`);
  const status = Object.assign(document.createElement("div"), { className: "status" });
  const show = (stop) => {
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
  return box;
}
function viaField(f, id) {
  const box = Object.assign(document.createElement("div"), { className: "station" });
  const chips = Object.assign(document.createElement("div"), { className: "chips" });
  const names = hidden(f.name);
  const ids = hidden(`${f.name}_id`);
  const stops = [...picked(SPEC.values, f.name)];
  const render = () => {
    names.value = stops.map((s) => s.name).join(";");
    ids.value = stops.map((s) => s.id).join(";");
    chips.replaceChildren(...stops.map((s, i) => {
      const chip = Object.assign(document.createElement("span"), { className: "chip", textContent: s.name });
      const remove = Object.assign(document.createElement("button"), {
        type: "button",
        textContent: "×",
        title: `Remove ${s.name}`
      });
      remove.addEventListener("click", () => {
        stops.splice(i, 1);
        render();
      });
      chip.append(remove);
      return chip;
    }));
  };
  const input = stationText(id, (stop) => {
    if (stop !== null) {
      stops.push(stop);
      input.value = "";
      render();
    }
    input.setCustomValidity(input.value.trim() === "" ? "" : `${NOT_PICKED}, or clear the field`);
  });
  input.placeholder = "Add a station";
  box.append(chips, input, names, ids);
  render();
  for (const name of unpicked(SPEC.values, f.name)) {
    const q = stationSearch(name);
    if (q !== null)
      lookup(q).then(() => {
        const stop = pickedNow(name);
        if (stop !== null) {
          stops.push(stop);
          render();
        }
      });
  }
  return box;
}
function fieldRow(f) {
  const row = document.createElement("div");
  row.className = "field";
  const id = `f-${f.name}`;
  const value = initial(f, SPEC.values, new Date);
  let control;
  if (f.kind === "station") {
    control = stationField(f, id);
  } else if (f.kind === "stations") {
    control = viaField(f, id);
  } else if (typeof value === "boolean") {
    control = Object.assign(document.createElement("input"), { type: "checkbox", checked: value, id, name: f.name });
  } else if (f.kind === "choice") {
    const select = Object.assign(document.createElement("select"), { id, name: f.name });
    for (const c of f.choices)
      select.append(new Option(c, c, false, c === value));
    control = select;
  } else {
    const text = Object.assign(document.createElement("input"), { id, name: f.name, value });
    text.type = f.kind === "number" ? "number" : f.kind === "time" ? "datetime-local" : "text";
    if (f.kind === "number")
      text.step = "any";
    control = text;
  }
  const l = Object.assign(document.createElement("label"), { htmlFor: id, textContent: label(f.name) });
  const help = Object.assign(document.createElement("div"), { className: "help", textContent: f.help });
  row.append(l, control, help);
  return row;
}
var trip = document.createElement("fieldset");
trip.append(Object.assign(document.createElement("legend"), { textContent: "Trip" }));
var limits = document.createElement("fieldset");
limits.append(Object.assign(document.createElement("legend"), { textContent: "Limits and display" }));
var advanced = document.createElement("details");
advanced.append(Object.assign(document.createElement("summary"), { textContent: "Advanced" }));
var placed = sections(SPEC.fields);
trip.append(...placed.trip.map(fieldRow));
limits.append(...placed.limits.map(fieldRow));
advanced.append(...placed.advanced.map(fieldRow));
var submit = Object.assign(document.createElement("button"), { type: "submit", textContent: "Search" });
var note = Object.assign(document.createElement("p"), {
  className: "note",
  textContent: "A search takes from 20 seconds to a few minutes; repeating one within 10 minutes is fast (cached)."
});
form.append(trip, limits, advanced, submit, note);
advanced.open = advancedOpen(SPEC.fields, SPEC.values);
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
      if (element instanceof HTMLInputElement || element instanceof HTMLSelectElement)
        element.disabled = false;
    }
  });
});
