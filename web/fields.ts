// The search form's fields without the DOM: their description from the
// server, where they go, what they start with and what is sent.

import { type Decoder, array, boolean, field, nullable, object, oneOf, record, string } from "./decode";
import { item } from "./util";

const kinds = ["text", "number", "flag", "choice", "time", "station", "stations"] as const;
export type Kind = (typeof kinds)[number];

export interface Field {
  name: string;
  help: string;
  kind: Kind;
  default: string;
  choices: string[];
  advanced: boolean;
}

export interface Spec {
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

export const decodeSpec: Decoder<Spec> = (v, path) => {
  const o = object(v, path);
  return {
    fields: field(o, "fields", array(decodeField), path),
    values: field(o, "values", record(string), path),
    error: field(o, "error", nullable(string), path),
  };
};

/** Option name as a label: "max_changes" → "Max changes". */
// Stryker disable next-line Regex: without the g flag /./ replaces only the first character too.
export const label = (name: string): string => name.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

/** The value given in the URL, if any (own keys only: "constructor" is no value). */
export const given = (values: Record<string, string>, name: string): string | undefined =>
  Object.hasOwn(values, name) ? values[name] : undefined;

const pad = (n: number): string => String(n).padStart(2, "0");
/** A local time as a datetime-local input value: "2026-10-25T08:05". */
export function localDateTime(d: Date): string {
  return `${String(d.getFullYear())}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * What a field starts with: a flag is set only if given (its default is
 * irrelevant), a time not given is now, anything else is the given value or
 * the default.
 */
export function initial(f: Field, values: Record<string, string>, now: Date): string | boolean {
  const value = given(values, f.name);
  if (f.kind === "flag") return value !== undefined;
  if (f.kind === "time") return value ?? localDateTime(now);
  return value ?? f.default;
}

// Where fields go: the trip first, then limits, the rest folded away.
const TRIP = ["from", "to", "at", "window", "via"];
/** Fields by section: the trip in its fixed order, then limits and advanced in spec order. */
export function sections(fields: readonly Field[]): { trip: Field[]; limits: Field[]; advanced: Field[] } {
  const trip = TRIP.flatMap((name) => fields.filter((f) => f.name === name).slice(0, 1));
  const rest = fields.filter((f) => !TRIP.includes(f.name));
  return { trip, limits: rest.filter((f) => !f.advanced), advanced: rest.filter((f) => f.advanced) };
}

/** Advanced settings that differ from the defaults stay visible. */
export const advancedOpen = (fields: readonly Field[], values: Record<string, string>): boolean =>
  fields.some((f) => f.advanced && given(values, f.name) !== undefined && given(values, f.name) !== f.default);

/**
 * Whether a field's value is left out of the submitted search: only what
 * differs from the default is sent (shorter links, same search). Flags and
 * required stations are always sent as they are.
 */
export const omitted = (f: Field, value: string): boolean =>
  f.kind !== "flag" && f.kind !== "station" && value === f.default;

/** A station picked from the suggestions: its name and Transitous id. */
export interface Stop {
  name: string;
  id: string;
}

const decodeStop: Decoder<Stop> = (v, path) => {
  const o = object(v, path);
  return { name: field(o, "name", string, path), id: field(o, "id", string, path) };
};

/** The server's suggestions for a station input. */
export const decodeStops: Decoder<Stop[]> = array(decodeStop);

/** What to look up for a station input: the trimmed text, null below 3 characters. */
export function stationSearch(text: string): string | null {
  const q = text.trim();
  return q.length < 3 ? null : q;
}

const split = (list: string): string[] =>
  list
    .split(";")
    .map((s) => s.trim())
    .filter(Boolean);

/**
 * Stations already picked, from the URL (a repeated or changed search): the
 * names of `name` with their ids from `name_id`, ";"-separated for lists.
 * Names without matching ids aren't picked yet.
 */
export function picked(values: Record<string, string>, name: string): Stop[] {
  const names = split(given(values, name) ?? "");
  const ids = split(given(values, `${name}_id`) ?? "");
  if (names.length !== ids.length) return [];
  return names.map((n, i) => ({ name: n, id: item(ids, i) }));
}

/** Names given in the URL without ids: to look up and pick if the name matches exactly. */
export function unpicked(values: Record<string, string>, name: string): string[] {
  return picked(values, name).length > 0 ? [] : split(given(values, name) ?? "");
}
