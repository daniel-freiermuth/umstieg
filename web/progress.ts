// Progress page of a running search (`umstieg serve`): polls the job's status
// and opens the plot when it is done.

import { parseJson } from "./decode";
import { byId } from "./dom";
import { decodeStatus, next } from "./status";

const log = byId("log", HTMLPreElement);
const title = byId("title", HTMLHeadingElement);
const back = byId("back", HTMLAnchorElement);
// The page lives at /job/{id}.
const job = location.pathname.replace(/\/+$/, "");

async function poll(): Promise<void> {
  const response = await fetch(`${job}/status`);
  if (!response.ok) {
    title.textContent = "Search not found";
    return;
  }
  const status = decodeStatus(parseJson(await response.text()), "status");
  back.href = status.back;
  log.textContent = status.log.join("\n");
  const then = next(status);
  if ("poll" in then) {
    setTimeout(() => void poll(), 1000);
    return;
  }
  if ("plot" in then) {
    location.replace(`${job}/plot`);
    return;
  }
  title.textContent = "No plot";
  const error = Object.assign(document.createElement("pre"), { className: "error", textContent: then.error });
  log.after(error);
}

void poll();
