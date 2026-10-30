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

// status.ts
var decodeStatus = (v, path) => {
  const o = object(v, path);
  return {
    log: field(o, "log", array(string), path),
    now: field(o, "now", nullable(string), path),
    done: field(o, "done", boolean, path),
    error: field(o, "error", nullable(string), path),
    no_connection: field(o, "no_connection", nullable(string), path),
    back: field(o, "back", string, path)
  };
};
var decodeProgress = (v, path) => ({
  status: field(object(v, path), "status", string, path)
});
function next(status) {
  if (!status.done)
    return { poll: true };
  if (status.error !== null)
    return { error: status.error };
  if (status.no_connection !== null)
    return { noConnection: status.no_connection };
  return { plot: true };
}

// progress.ts
var log = byId("log", HTMLPreElement);
var now = byId("now", HTMLPreElement);
var title = byId("title", HTMLHeadingElement);
var back = byId("back", HTMLAnchorElement);
var { status: statusUrl } = decodeProgress(parseJson(byId("data", HTMLScriptElement).text), "data");
async function poll() {
  let status;
  try {
    const response = await fetch(statusUrl);
    if (response.status === 404) {
      document.body.classList.add("over");
      title.textContent = "Search not found";
      return;
    }
    if (!response.ok)
      throw new Error(`status ${String(response.status)}`);
    status = decodeStatus(parseJson(await response.text()), "status");
  } catch {
    setTimeout(() => void poll(), 1000);
    return;
  }
  back.href = status.back;
  log.textContent = status.log.join(`
`);
  now.textContent = status.now ?? "";
  const then = next(status);
  if ("poll" in then) {
    setTimeout(() => void poll(), 1000);
    return;
  }
  if ("plot" in then) {
    location.reload();
    return;
  }
  document.body.classList.add("over");
  const [heading, className, text] = "error" in then ? ["No plot", "error", then.error] : ["No connection found", "report", then.noConnection];
  title.textContent = heading;
  log.after(Object.assign(document.createElement("pre"), { className, textContent: text }));
}
poll();
