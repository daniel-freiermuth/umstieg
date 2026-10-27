// Progress page of a running search (`umstieg serve`), shown at the search's
// own link: polls the job's status and loads the link again when the plot is
// ready, which then shows it.

import { parseJson } from "./decode";
import { byId } from "./dom";
import { decodeProgress, decodeStatus, next } from "./status";

const log = byId("log", HTMLPreElement);
const now = byId("now", HTMLPreElement);
const title = byId("title", HTMLHeadingElement);
const back = byId("back", HTMLAnchorElement);
const { status: statusUrl } = decodeProgress(parseJson(byId("data", HTMLScriptElement).text), "data");

async function poll(): Promise<void> {
  const response = await fetch(statusUrl);
  if (!response.ok) {
    title.textContent = "Search not found";
    return;
  }
  const status = decodeStatus(parseJson(await response.text()), "status");
  back.href = status.back;
  log.textContent = status.log.join("\n");
  // The current stage's progress, under the log, replaced as it comes.
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
  title.textContent = "No plot";
  const error = Object.assign(document.createElement("pre"), { className: "error", textContent: then.error });
  log.after(error);
}

void poll();
