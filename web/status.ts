// Status of a running search (`umstieg serve`), as polled by the progress page.

import { type Decoder, array, boolean, field, nullable, object, string } from "./decode";

export interface Status {
  log: string[];
  /** How the current stage is getting on, if it said. */
  now: string | null;
  done: boolean;
  /** Why there is no plot: an error, or the report saying nothing was found. */
  error: string | null;
  back: string;
}

export const decodeStatus: Decoder<Status> = (v, path) => {
  const o = object(v, path);
  return {
    log: field(o, "log", array(string), path),
    now: field(o, "now", nullable(string), path),
    done: field(o, "done", boolean, path),
    error: field(o, "error", nullable(string), path),
    back: field(o, "back", string, path),
  };
};

/** What the progress page is given: where to poll the search's status. */
export const decodeProgress: Decoder<{ status: string }> = (v, path) => ({
  status: field(object(v, path), "status", string, path),
});

/** What the page does next: poll again, open the plot, or show why there is none. */
export type Next = { poll: true } | { plot: true } | { error: string };
export function next(status: Status): Next {
  if (!status.done) return { poll: true };
  return status.error === null ? { plot: true } : { error: status.error };
}
