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

// progress.ts
var decodeStatus = (v, path) => {
  const o = object(v, path);
  return {
    log: field(o, "log", array(string), path),
    done: field(o, "done", boolean, path),
    error: field(o, "error", nullable(string), path),
    back: field(o, "back", string, path)
  };
};
var log = byId("log", HTMLPreElement);
var title = byId("title", HTMLHeadingElement);
var back = byId("back", HTMLAnchorElement);
var job = location.pathname.replace(/\/+$/, "");
async function poll() {
  const response = await fetch(`${job}/status`);
  if (!response.ok) {
    title.textContent = "Search not found";
    return;
  }
  const status = decodeStatus(parseJson(await response.text()), "status");
  back.href = status.back;
  log.textContent = status.log.join(`
`);
  if (!status.done) {
    setTimeout(() => void poll(), 1000);
    return;
  }
  if (status.error === null) {
    location.replace(`${job}/plot`);
    return;
  }
  title.textContent = "No plot";
  const error = Object.assign(document.createElement("pre"), { className: "error", textContent: status.error });
  log.after(error);
}
poll();
