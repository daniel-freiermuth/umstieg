// JSON from the server, checked into typed values. `JSON.parse` is typed
// `any`; `parseJson` is the one place that looks at what it returns (as
// `unknown`, the honest type for unchecked input) and turns it into `Json`.
// Everything else decodes `Json` with the decoders below, so no value is
// typed by assertion.

export type Json = string | number | boolean | null | readonly Json[] | JsonObject;
export interface JsonObject {
  readonly [key: string]: Json;
}

export class DecodeError extends Error {}

function fail(path: string, expected: string): never {
  throw new DecodeError(`${path}: expected ${expected}`);
}

function toJson(value: unknown, path: string): Json {
  if (value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return value;
  }
  if (Array.isArray(value)) return value.map((x: unknown, i) => toJson(x, `${path}[${String(i)}]`));
  if (typeof value === "object") {
    const out: Record<string, Json> = {};
    for (const [key, x] of Object.entries(value)) out[key] = toJson(x, `${path}.${key}`);
    return out;
  }
  return fail(path, "JSON");
}

export function parseJson(text: string): Json {
  const value: unknown = JSON.parse(text);
  return toJson(value, "$");
}

export type Decoder<T> = (value: Json, path: string) => T;

const isArray = (value: Json): value is readonly Json[] => Array.isArray(value);
const isObject = (value: Json): value is JsonObject => typeof value === "object" && value !== null && !isArray(value);

export const string: Decoder<string> = (v, path) => (typeof v === "string" ? v : fail(path, "a string"));
export const number: Decoder<number> = (v, path) => (typeof v === "number" ? v : fail(path, "a number"));
export const boolean: Decoder<boolean> = (v, path) => (typeof v === "boolean" ? v : fail(path, "a boolean"));

export const nullable =
  <T>(decode: Decoder<T>): Decoder<T | null> =>
  (v, path) =>
    v === null ? null : decode(v, path);

export const array =
  <T>(decode: Decoder<T>): Decoder<T[]> =>
  (v, path) =>
    isArray(v) ? v.map((x, i) => decode(x, `${path}[${String(i)}]`)) : fail(path, "an array");

export const pair =
  <A, B>(first: Decoder<A>, second: Decoder<B>): Decoder<[A, B]> =>
  (v, path) => {
    if (!isArray(v) || v.length !== 2) return fail(path, "a pair");
    const [a, b] = v;
    if (a === undefined || b === undefined) return fail(path, "a pair");
    return [first(a, `${path}[0]`), second(b, `${path}[1]`)];
  };

export const oneOf =
  <T extends string>(...options: readonly T[]): Decoder<T> =>
  (v, path) =>
    options.find((o) => o === v) ?? fail(path, `one of ${options.join(", ")}`);

/** A JSON object, to read fields from with `field`. */
export const object = (v: Json, path: string): JsonObject => (isObject(v) ? v : fail(path, "an object"));

/** Field `name` of an object, decoded; missing fields fail. */
export function field<T>(o: JsonObject, name: string, decode: Decoder<T>, path: string): T {
  const v = o[name];
  return v === undefined ? fail(`${path}.${name}`, "a value") : decode(v, `${path}.${name}`);
}

/** Every value of a JSON object, decoded. */
export const record =
  <T>(decode: Decoder<T>): Decoder<Record<string, T>> =>
  (v, path) => {
    const o = object(v, path);
    const out: Record<string, T> = {};
    for (const [key, x] of Object.entries(o)) out[key] = decode(x, `${path}.${key}`);
    return out;
  };
