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

// form.ts
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
var SPEC = decodeSpec(parseJson(byId("spec", HTMLScriptElement).text), "spec");
var form = byId("form", HTMLFormElement);
if (SPEC.error !== null)
  Object.assign(byId("error", HTMLDivElement), { textContent: SPEC.error, hidden: false });
var TRIP = ["from", "to", "at", "window", "via"];
var label = (name) => name.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
var pad = (n) => String(n).padStart(2, "0");
var now = () => {
  const d = new Date;
  return `${String(d.getFullYear())}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
};
var given = (name) => Object.hasOwn(SPEC.values, name) ? SPEC.values[name] : undefined;
function fieldRow(f) {
  const row = document.createElement("div");
  row.className = "field";
  const id = `f-${f.name}`;
  const value = given(f.name) ?? f.default;
  let input;
  if (f.kind === "flag") {
    input = Object.assign(document.createElement("input"), { type: "checkbox", checked: given(f.name) !== undefined });
  } else if (f.kind === "choice") {
    input = document.createElement("select");
    for (const c of f.choices)
      input.append(new Option(c, c, false, c === value));
  } else {
    const text = document.createElement("input");
    text.type = f.kind === "number" ? "number" : f.kind === "time" ? "datetime-local" : "text";
    if (f.kind === "number")
      text.step = "any";
    text.value = f.kind === "time" && given(f.name) === undefined ? now() : value;
    if (f.kind === "station" || f.kind === "stations")
      stationInput(text, f.kind === "stations");
    if (f.kind === "station")
      text.required = true;
    if (f.kind === "stations")
      text.placeholder = "Several separated by ;";
    input = text;
  }
  Object.assign(input, { id, name: f.name });
  const l = Object.assign(document.createElement("label"), { htmlFor: id, textContent: label(f.name) });
  const help = Object.assign(document.createElement("div"), { className: "help", textContent: f.help });
  row.append(l, input, help);
  return row;
}
var lists = 0;
function stationInput(input, several) {
  const list = Object.assign(document.createElement("datalist"), { id: `stops-${String(lists++)}` });
  input.setAttribute("list", list.id);
  input.autocomplete = "off";
  document.body.append(list);
  let timer;
  input.addEventListener("input", () => {
    clearTimeout(timer);
    const parts = input.value.split(";");
    const q = (several ? parts.at(-1) ?? "" : input.value).trim();
    const prefix = several ? parts.slice(0, -1).map((s) => s.trim()).filter(Boolean).map((s) => `${s}; `).join("") : "";
    if (q.length < 3)
      return;
    timer = setTimeout(() => {
      (async () => {
        const response = await fetch(`/stops?q=${encodeURIComponent(q)}`);
        if (!response.ok)
          return;
        const names = array(string)(parseJson(await response.text()), "stops");
        list.replaceChildren(...names.map((n) => new Option(prefix + n)));
      })();
    }, 250);
  });
}
var trip = document.createElement("fieldset");
trip.append(Object.assign(document.createElement("legend"), { textContent: "Trip" }));
var limits = document.createElement("fieldset");
limits.append(Object.assign(document.createElement("legend"), { textContent: "Limits and display" }));
var advanced = document.createElement("details");
advanced.append(Object.assign(document.createElement("summary"), { textContent: "Advanced" }));
for (const name of TRIP) {
  const f = SPEC.fields.find((f2) => f2.name === name);
  if (f)
    trip.append(fieldRow(f));
}
for (const f of SPEC.fields) {
  if (TRIP.includes(f.name))
    continue;
  (f.advanced ? advanced : limits).append(fieldRow(f));
}
var submit = Object.assign(document.createElement("button"), { type: "submit", textContent: "Search" });
var note = Object.assign(document.createElement("p"), {
  className: "note",
  textContent: "A search takes from 20 seconds to a few minutes; repeating one within 10 minutes is fast (cached)."
});
form.append(trip, limits, advanced, submit, note);
if (SPEC.fields.some((f) => f.advanced && given(f.name) !== undefined && given(f.name) !== f.default)) {
  advanced.open = true;
}
form.addEventListener("submit", () => {
  for (const f of SPEC.fields) {
    const element = form.elements.namedItem(f.name);
    const fixed = f.kind === "flag" || f.kind === "station";
    if (!fixed && (element instanceof HTMLInputElement || element instanceof HTMLSelectElement)) {
      if (element.value === f.default)
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
