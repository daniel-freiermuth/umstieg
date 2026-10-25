// Small collection helpers shared by the plot modules.

/** Element i, which the data guarantees to exist. */
export function item<T>(xs: readonly T[], i: number): T {
  const x = xs[i];
  if (x === undefined || i < 0) throw new Error(`index ${String(i)} out of range`);
  return x;
}

/** The entry of k, created by make if missing. */
export function entry<K, V>(m: Map<K, V>, k: K, make: () => V): V {
  let v = m.get(k);
  if (v === undefined) {
    v = make();
    m.set(k, v);
  }
  return v;
}
