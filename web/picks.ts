// Narrowing down by picks, without the DOM: clicked lines and stations, the
// connections passing all of them, and what is reachable through each.

import type { XY } from "./geometry";
import { type Reach, intersectReach } from "./reach";
import { item } from "./util";

export interface Picked {
  id: string;
  info: string;
  /** Where it is drawn: a stretch, a stay, or a station's row. */
  pts: readonly XY[];
  chains: number[];
  reach: Reach | null;
}
export interface Candidates {
  info: string;
  chains: number[];
  reach: Reach | null;
}

/**
 * The connections passing every pick (in the first pick's order, hidden ones
 * left out) and the highlight reachable through every pick that has one:
 * per piece, the stretch all their highlights share. None without picks.
 */
export function narrow(picks: readonly Picked[], visible: (chain: number) => boolean): Candidates | null {
  const [first, ...rest] = picks;
  if (!first) return null;
  let chains = first.chains.filter(visible);
  for (const p of rest) {
    const s = new Set(p.chains);
    chains = chains.filter((i) => s.has(i));
  }
  const reaches = picks.map((p) => p.reach).filter((r): r is Reach => r !== null);
  const reach = reaches.length
    ? {
        key: picks.map((p) => p.reach?.key ?? p.id).join("|"),
        segments: intersectReach(reaches.map((r) => r.segments)),
      }
    : null;
  return { info: picks.map((p) => p.info).join(" + "), chains, reach };
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
