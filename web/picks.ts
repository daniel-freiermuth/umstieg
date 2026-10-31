// Narrowing down by picks, without the DOM: clicked lines and stations, what
// connections must pass, and what is reachable through each.

import type { Need } from "./best";
import type { XY } from "./geometry";
import { type Reach, intersectReach } from "./reach";
import { item } from "./util";

export interface Picked {
  id: string;
  info: string;
  /** Where it is drawn: a stretch, a stay, or a station's row. */
  pts: readonly XY[];
  /** What a connection passing it must do there. */
  need: Need | null;
  reach: Reach | null;
}
/** Together, the picks: their names, what connections must pass, the shared highlight. */
export interface Narrowed {
  info: string;
  needs: Need[];
  reach: Reach | null;
}

/**
 * What connections must pass to pass every pick, and the highlight
 * reachable through every pick that has one: per piece, the stretch all their
 * highlights share. Nothing without picks.
 */
export function narrow(picks: readonly Picked[]): Narrowed | null {
  if (picks.length === 0) return null;
  const reaches = picks.map((p) => p.reach).filter((r): r is Reach => r !== null);
  const reach = reaches.length
    ? {
        key: picks.map((p) => p.reach?.key ?? p.id).join("|"),
        segments: intersectReach(reaches.map((r) => r.segments)),
      }
    : null;
  const needs = picks.flatMap((p) => (p.need === null ? [] : [p.need]));
  return { info: picks.map((p) => p.info).join(" + "), needs, reach };
}

/** The selection after the candidates change: the only one, else the old one if still listed. */
export function pinAfter(chains: readonly number[], pinned: number | null): number | null {
  if (chains.length === 1) return item(chains, 0);
  return pinned !== null && chains.includes(pinned) ? pinned : null;
}

/**
 * The selection after stepping through the list (↓ or ↑), wrapping around;
 * the first one when nothing listed is selected, none in an empty list.
 */
export function step(list: readonly number[], pinned: number | null, down: boolean): number | null {
  const now = pinned === null ? -1 : list.indexOf(pinned);
  const next = now < 0 ? 0 : (now + (down ? 1 : list.length - 1)) % list.length;
  return list[next] ?? null;
}

/** Picks with the target toggled: removed if picked (by id), else appended. */
export function toggle(picks: readonly Picked[], target: Picked): Picked[] {
  return picks.some((p) => p.id === target.id) ? picks.filter((p) => p.id !== target.id) : [...picks, target];
}
