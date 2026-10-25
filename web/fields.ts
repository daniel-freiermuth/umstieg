// The search form's fields without the DOM: their description from the
// server, where they go, what they start with and what is sent.

import { type Decoder, array, boolean, field, nullable, object, oneOf, record, string } from "./decode";

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

/**
 * What to complete in a station input: the whole value, or for lists the
 * part after the last ";" with the earlier names kept as prefix of every
 * suggestion. Null below 3 characters.
 */
export function stationQuery(value: string, several: boolean): { q: string; prefix: string } | null {
  const parts = value.split(";");
  // Stryker disable next-line StringLiteral: split always returns at least one part, so the fallback is never used.
  const q = (several ? (parts.at(-1) ?? "") : value).trim();
  const prefix = several
    ? parts
        .slice(0, -1)
        .map((s) => s.trim())
        .filter(Boolean)
        .map((s) => `${s}; `)
        .join("")
    : "";
  return q.length < 3 ? null : { q, prefix };
}
