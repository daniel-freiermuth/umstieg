// Progress page of a running search (`umstieg serve`), shown at the search's
// own link: polls the job's status and loads the link again when the plot is
// ready, which then shows it.

import { parseJson } from "./decode";
import { byId } from "./dom";
import { type Status, decodeProgress, decodeStatus, next } from "./status";

const log = byId("log", HTMLPreElement);
const now = byId("now", HTMLPreElement);
const title = byId("title", HTMLHeadingElement);
const back = byId("back", HTMLAnchorElement);
const { status: statusUrl } = decodeProgress(parseJson(byId("data", HTMLScriptElement).text), "data");

async function poll(): Promise<void> {
  let status: Status;
  try {
    const response = await fetch(statusUrl);
    if (!response.ok) {
      document.body.classList.add("over");
      title.textContent = "Search not found";
      return;
    }
    status = decodeStatus(parseJson(await response.text()), "status");
  } catch {
    // No status this time (a network blip, the laptop asleep, a proxy's page
    // instead): the search runs on, so ask again.
    setTimeout(() => void poll(), 1000);
    return;
  }
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
  document.body.classList.add("over");
  const [heading, className, text] =
    "error" in then ? ["No plot", "error", then.error] : ["No connection found", "report", then.noConnection];
  title.textContent = heading;
  log.after(Object.assign(document.createElement("pre"), { className, textContent: text }));
}

void poll();
