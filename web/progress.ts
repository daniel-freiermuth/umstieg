// Progress page of a running search (`umstieg serve`): polls the job's status
// and opens the plot when it is done.

interface Status {
  log: string[];
  done: boolean;
  /** Why there is no plot: an error, or the report saying nothing was found. */
  error: string | null;
  back: string;
}

function byId<T extends HTMLElement>(id: string, type: new () => T): T {
  const element = document.getElementById(id);
  if (!(element instanceof type)) throw new Error(`missing #${id}`);
  return element;
}

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
  const status = (await response.json()) as Status;
  back.href = status.back;
  log.textContent = status.log.join("\n");
  if (!status.done) {
    setTimeout(() => void poll(), 1000);
    return;
  }
  if (status.error === null) {
    location.replace(`${job}/plot`);
    return;
  }
  title.textContent = "No plot";
  const error = Object.assign(document.createElement("pre"), { className: "error", textContent: status.error });
  log.after(error);
}

void poll();
