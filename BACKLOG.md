# Backlog

Ideas and deferred work, roughly grouped. Not promises.

## Finding trains the planner hides, outside Germany and Sweden

In Germany DB's station boards, in Sweden Trafikverket's (with whole runs and
junction boards), bring every train at a corridor station into the timetable.
Elsewhere it only holds the trains of Transitous's answers, and the planner
drops a departure a better one dominates. Example from before Trafikverket was
used: Göteborg → Karlshamn on
2026-10-29, 06:00–08:00: SJ lists SJ Regional 319 at 06:04 (→ Alvesta, Krösatåg,
Öresundståg) and SJ 3000 481 at 06:49; we find only the Öresundståg via Lund,
the single hub. Both trains are in Transitous.

- **A. Transitous departures as boards**: `stoptimes` (rail only) at the
  corridor stations by the hour, trains joined across stations by trip id, as
  with DB. Finds 481 (Göteborg → Lund); a few dozen requests per search. A
  departure names only the destination, not the stops, so stations no answer
  mentions (Alvesta, Emmaboda) stay unknown.
- **B. Hubs from a wider search**: one extra request with a day-wide window,
  only to collect change stations (the night answers bring Hässleholm,
  Kristianstad, Helsingborg). No key needed, helps everywhere.
- **C. Whole routes of board trains** (`trip`): every station a train calls
  at, which reaches Alvesta and Emmaboda. A few hundred requests per search;
  only workable for long-distance trains and a few per board.

## More railways' data

- **Denmark: Rejseplanen API 2.0** (key requested). `departureBoard` and
  `journeyDetail` with live times for all Danish trains; 50,000 calls a month.
- **Denmark without a key**: live times of cross-border trains at Danish stops
  from their DELFI copy in Transitous, which carries realtime there (IC 5764 at
  Kolding), where DB's board has none.

## Times

- **Check every time against DB**: today a time comes from DB only where a
  board was fetched (where the rough routes' trains start or end, plus stations
  found along DB routes); intermediate Transitous stops and stops filled in
  after the search keep Transitous's time plus the carried delay. Fetching the
  boards for them costs about 1–2 minutes of DB quota per search
  (Göteborg → Husum: ~135 requests). `--all-stops` already does most of it.

## Plot page

- **Per-piece instead of per-connection**: `reach.ts` `network()` builds
  17.5M ride memberships and 9.6M wait spans for 188k connections (~16 s);
  Frankfurt Süd → Hamburg with 276k connections didn't finish drawing in
  10 minutes. Compute once per distinct ride or wait.
- **Canvas instead of SVG**, with own hit-testing (`distanceToLine` exists);
  WebGL beyond that. Calculation in a Web Worker; less detail when zoomed out.
- **Headless-browser tests** for the DOM code (form, pointer handling, plot
  interaction), which is checked by hand today.

## Search page

- **Send all form values in links**, not only those differing from the
  defaults, so a changed default doesn't change old links.
- **Via without an id** (an older link): `via=Hamburg+Hbf` showed no chip.
- **Connections that start with a walk**.

## Running it

- **Deploy on entrop**: no Node/Bun there, so build from the committed
  `web/dist`; systemd unit `ExecStart=/usr/local/bin/umstieg serve --addr
  127.0.0.1:8765 --parallel 1 --contact …`; Caddy `umstieg.freiermuth.dev`
  with basic auth; copy `db.env`.
- **Drop `--html`** (static file output) once the server covers it.
