// DOM lookups checked for presence and element type.

export function byId<T extends HTMLElement>(id: string, type: new () => T): T {
  const element = document.getElementById(id);
  if (!(element instanceof type)) throw new Error(`#${id} missing`);
  return element;
}
