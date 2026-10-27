# umstieg

Lists every public transport connection within limits — including the ones
journey planners hide — grouped by first and last train.

Planners return only the Pareto set (departure, arrival, changes) and drop
everything else, e.g. a trip that waits longer at a change for the same final
train. Open timetables can also be wrong: DELFI, the source behind Transitous
and gtfs.de, routes ICE 378 via Frankfurt Hbf, while DB runs it via Frankfurt
Süd. `umstieg` builds its own small timetable and enumerates.

```
$ umstieg "Mannheim Hbf" "København H" --at "2026-10-24 06:30"
fastest 9h39 · limits: ≤ 12h39 travel time, ≤ 15 changes · 1090 connections in 8 groups

■ ICE 378 … RJ 384  (49)
  06:39 CEST → 17:42 CEST  11h03  2×  min 42m   06:39 CEST (+1) ICE 378 → 07:15 CEST (+2) Frankfurt (Main) Südbahnhof ‖49m‖ 08:04 CEST (+12) ICE 2558 + ICE 76 → 11:51 CEST Hamburg Hbf ‖42m‖ 12:33 CEST (+5) RJ 384 → 17:42 CEST (+4) København H
  06:39 CEST → 17:42 CEST  11h03  2×  min 36m   06:39 CEST (+1) ICE 378 → 08:21 CEST (+10) Fulda Bahnhof ‖48m‖ 09:09 CEST (+5) ICE 886 → 11:57 CEST Hamburg Hbf ‖36m‖ 12:33 CEST (+5) RJ 384 → 17:42 CEST (+4) København H
  …
```

`‖42m‖` is the buffer at a change, `⚠` marks buffers below `--min-transfer`,
`(+5)` live delays. `‖missed 2m✗‖` is a change live data says is gone by that
much: `--missed-by` (default 5 min, 0 = off) lists those too, since delays can
still shift either way; they sort after the feasible connections of their
group, and their fallbacks show what still works. `umstieg --help` lists all options.

Times are in `--time-zone` (e.g. `Europe/Berlin`; default: this machine's, from
`TZ` or the system), both `--at` as given and every time shown, and each shown
time names its zone (`06:39 CEST`).

`--detail` adds platforms and, for every change, up to three fallbacks if the
next train is missed: connections leaving within 2 h with at most 3 changes,
cut where they catch a train of the plan again ("back on plan"), e.g.

```
    08:04 CEST (+12) Frankfurt (Main) Südbahnhof [5] → 11:52 CEST (+1) Hamburg Hbf [12]   ICE 2558 + ICE 76
      ↳ 48m buffer · if missed:
          12:42 CEST S1 (41618) → 12:54 CEST Altona ‖14m‖ 13:08 CEST RJ 384 (back on plan) · arrive 22:05 CEST (+0m)
          12:52 CEST S1 (41620) → 13:04 CEST Altona ‖4m⚠‖ 13:08 CEST RJ 384 (back on plan) · arrive 22:05 CEST (+0m)
```

### Plot

`--html out.html` writes a self-contained interactive time–distance diagram
(a Marey chart generalised to a network): x is time, y a fixed height per
station with the goal at the bottom. Every stretch is drawn once, however many
connections use it: riding in one colour per train (follow a train across all
connections), waiting as a thin horizontal line (merged per continuous stay;
red below `--min-transfer`), walking dotted. Later departures of the same route
run parallel, faster trains are steeper and overtake slower ones.

Hovering only previews: it highlights what is reachable through the point under
the mouse and lists the connections there by group. On a flat line at a station
(waiting, or in a train standing there; one target per continuous stay, nothing
stacks) that is every connection arriving by then, up to the station, and every
one leaving from then on, from the station, later departures included. On a
ride it is every connection on that train between those two stops, plus what
could lead onto the train (arriving at an earlier stop by its departure there)
and what it leads to (leaving a later stop from its arrival there on).
Reachability is transitive over everything plotted, so changes no listed
connection makes count too (ICE 774 to Kassel, then ICE 772). Station names on
the axis work the same way: hovering shows every connection touching the
station and what is reachable through it at any time.

Clicking a line or station name picks it: picks are shown at the top of the side
panel, and clicking a pick (or its line again) removes it. The connections
passing every pick are listed below (e.g. 63 on ICE 378 Mannheim → Frankfurt
Süd; narrowed further by also picking Ludwigshafen Mitte); if one is left it is
selected. Hovering a listed connection previews it, clicking selects it. The
highlight is what is reachable through every pick; it and the other listed
connections fade to 30 % once one is selected or previewed.
`↑`/`↓` step through the list, `Esc` or clicking the background clears. The selection gets its changes
labelled and its fallbacks drawn dashed in train colours, starting with a
dotted wait from the missed departure; hovering a fallback stretch names it and
every fallback using it. Hovering a group (first … last train) highlights it, clicking
hides it.

The mouse wheel zooms around the pointer (`Shift`: time only, `Alt`: height
only), dragging pans, double-click or "Reset zoom" shows everything again.
Station labels and time ticks follow the zoom.

The station height is chosen in the page (`Height`); `--metric` sets the one
shown first. Stations without a height in a metric (no coordinates for the
distance ones, goal unreachable) are left out of it:

| Metric | Height | Equidistant lines |
|---|---|---|
| `remaining-time` (default) | fastest possible remaining ride time, ignoring waits: fastest ride per stretch (planned or live, whichever is shorter) from any train at any time, every stretch counted both ways | network-based; no ride or walk rises or falls more than it lasts (at most 45°, exactly 45° down on the fastest train), so a detour climbs gently instead of jumping where the timetable lacks the trains back; no single connection explains a height |
| `fastest-real` | duration of the fastest real journey from there, waits included, at any departure in the timetable | each height is a connection one can look up; riding can run flat or rise where connections are poor |
| `hyperbolic` | (d_goal − d_start + D) / 2 | hyperbolas, start and goal as foci |
| `axis` | distance left along the start–goal axis (from d_start² − d_goal²) | straight lines perpendicular to the axis |
| `ratio` | d_goal / (d_start + d_goal) | Apollonius circles |

With geographic metrics, riding sideways to the axis looks flat and moving away
from the goal goes up (e.g. København → Malmö on the way to Göteborg).

### In the browser

```sh
umstieg serve                     # http://127.0.0.1:8080/
umstieg serve --addr 127.0.0.1:8765 --parallel 1 --contact you@example.org
```

A form with every search option of the command line (generated from them, so
new options appear by themselves). Left out: text-only ones like `--detail`,
`--metric` (the plot page switches it), and the services and contact
(`--api`, `--efa`, `--contact`), which `umstieg serve` takes and uses for every
search; the page can't change them. Stations are chosen from Transitous's
suggestions as one types: a field shows "✓ found" once its text names one
exactly, and the search can't start before every station is chosen (vias become
removable chips). The chosen stop's id goes with the search, so it runs from
exactly that stop, not from the best match for the name; the command line
still takes names. The time zone is the browser's, filled in next to the time
(changeable): the search and its plot are in the user's time, whatever zone
the server runs in.
Submitting shows the search's
progress, then the plot, whose "← Change search" link returns to the form with
the same settings. The plot page carries the drawing; a connection's route and
fallbacks are fetched when it is shown (a night search can list 190 000
connections). Searches beyond `--parallel` wait their turn: each loads
Transitous and the DB quota. The last 20 results stay available. There is no
login; put it behind an authenticating proxy before exposing it.

Logs go to stderr (the journal under systemd), filtered by `RUST_LOG`. The
server logs at `info` by default: startup, each search asked, done (with its
duration) or failed (with the error), its warnings, and the requests it sent
per service plus time spent waiting for the DB quota. `RUST_LOG=umstieg=debug`
adds every request to Transitous, EFA and DB (URL, status, attempt, duration)
and cache hits; `RUST_LOG=debug` also every request to the server itself. The
command line logs nothing unless `RUST_LOG` is set.

## How it works

1. **Rough routes** (MOTIS/Transitous): the planner's answers, plus
   origin → hub and hub → destination halves for the stations where those
   answers change trains. They pick the corridor.
2. **Timetable** (DB Timetables API): hourly boards for every station where a
   rough route boards or leaves a train (`--all-stops`: every stop of those
   trains, about three times the requests), plus stations where DB long-distance
   trains call before the next corridor station (this finds Frankfurt Süd).
   Trains are joined across boards by DB's run id, live changes applied.
   `--planned` ignores delays (planned times only); DB's latest schedule —
   cancellations, diversions, extra trains — still applies.
   MOTIS trains are merged in by number or identical times; DB wins where it
   has data, MOTIS fills the rest (abroad, buses). Without DB (past days,
   abroad), copies of a train from Transitous and EFA become one by number;
   EFA's times win, and the other copy only adds stops where both agree on the
   stops around them (Transitous had ICE 273 22 minutes late at Mannheim).
   Stations from different feeds are unified by EVA number, name or distance
   (< 300 m).
3. **Enumeration**: depth-first over that timetable, deepening by number of
   changes, within these limits:
   - departure inside `--window`, travel time ≤ fastest + `--extra-hours`,
   - at most `--max-changes` changes, any transfer time (short ones flagged;
     missed by up to `--missed-by` listed as ✗),
   - no station twice (also not passing through). Riding away from the
     destination is allowed: S-Bahn Mannheim → Heidelberg for an ECE that skips
     Mannheim can be worth it. `--forward-only` requires every train to reduce
     the best-case remaining travel time (fewer results, ~8× less search),
   - no pointless detours: a train is not boarded if the chain already stopped
     at an earlier station where it could have been caught comfortably
     (≥ `--min-transfer`) with at least the smallest buffer the detour's changes
     leave — and never after leaving the origin before the same train
     (`--keep-detours` disables). Arriving at Hamburg Hbf 11:51 and taking the
     RE7 to Schleswig (11 min change) for the RJ that leaves Hamburg Hbf at
     12:38 is pruned; arriving 12:36 and taking the S1 to Altona (16 and 18 min
     changes) instead of a 14 min change to the same RJ stays, as it raises
     the worst buffer.

   Variants riding the same trains but changing at different stations are all
   kept (RNV 4 → S1 changing at Ludwigshafen Hbf or at Ludwigshafen Mitte).
   If the effort cap (`--max-nodes`) is hit, the output says up to how many
   changes the list is complete.

4. **Stops for the display**: a train only DB boards know (no Transitous
   answer rode it) has times only where boards were fetched, and looks like a
   slow nonstop train: FLX 1236 Frankfurt Süd → Hamburg Hbf via Fulda, Erfurt
   and Berlin. For up to 20 such trains in the listed connections, widest gaps
   first, Transitous' departures at a known stop and then the trip fill in the
   stops between (two requests each, cached for 10 minutes). Only where both
   agree on the times; the search has run already, so changes there aren't
   offered.

Without DB credentials, the DB part of step 2 is skipped and the timetable consists
of the MOTIS trains only.

## Data

MOTIS data comes from [Transitous](https://transitous.org)
([sources](https://transitous.org/sources/)), a volunteer-run instance. The
planner drops departures that a later one arrives as early as, so departure
windows are asked for in one-hour slices; a search makes roughly 50–100
requests, two in parallel, retrying on HTTP 429 and transport errors. Answers
are cached in `~/.cache/umstieg/motis/` for 10 minutes, so repeating or
tweaking a search costs nothing.
Set `UMSTIEG_CONTACT` for the User-Agent, as the
[usage policy](https://transitous.org/api/) requires. `--api` points at another
MOTIS instance.

Local legs between German stops are also asked from an EFA journey planner,
by default the VRN's (`--efa URL`, `--efa ''` disables). Transitous can miss
local services: on Saturday 24 October 2026 it had no trams at Bad Dürkheim,
while the VRN knew RNV 4 at 05:13. EFA is asked origin → hub, hub →
destination and origin → destination, hourly across the window, wherever both
ends have a German stop id (DHID, from Transitous's DELFI ids). Answers are
cached in `~/.cache/umstieg/efa/` for 10 minutes. DB's own journey planners
(bahn.de, DB Navigator) block automated access.

DB data comes from the [Timetables API](https://developers.deutschebahn.com)
(free plan; subscribe your application to it). Credentials come from
`DB_CLIENT_ID`/`DB_API_KEY` or `~/.config/umstieg/db.env` (`KEY=value` lines).
Requests stay under 60 per minute. Planned boards and station lookups are cached
in `~/.cache/umstieg/`; live changes are fetched on every run. `--no-db` skips
DB.

## Limits

- Connections through stations outside the corridor are not found. The
  corridor follows MOTIS's rough routes, which vary with live data; a change
  point like Schleswig is only covered if a rough route uses it or with
  `--all-stops`.
- Minimum transfer times per station are not available from these APIs; one
  global `--min-transfer` is used for flagging.
- DB boards cover German stations (and a few abroad); elsewhere times come from
  Transitous.

## Development

`just check` runs what must pass: `cargo fmt --check`, clippy with every lint
as an error, tests and rustdoc. The lint set lives in `Cargo.toml`: clippy's
`pedantic`, `nursery` and `cargo` groups plus restriction lints against
`unwrap`/`expect`/`panic`, stray `print`s and `allow`s without a reason.
Deliberate exceptions use `#[expect(…, reason = "…")]`, so they fail once they
no longer apply.

The code doesn't panic: `unwrap`/`expect`/`panic!`/`unreachable!`, indexing,
slicing and arithmetic that can overflow or divide by zero are lint errors
outside tests, with no exceptions. Bad feed data skips the item or fails the
search with an error, internal bounds saturate (`src/checked.rs`), and
command-line and form limits are range-checked (`--window` up to a week, …).

The pages' scripts are TypeScript in `web/` (`plot.ts`, `form.ts`,
`progress.ts`, sharing `decode.ts` and `dom.ts`; templates next to them),
bundled by Bun into `web/dist/`, which is committed and embedded by the binary —
building umstieg needs neither Node nor Bun. After changing them: `cd web &&
npm install` once, then `just web`. `just check` also type-checks them
(`strict` plus `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes` & co.),
lints with typescript-eslint's `strictTypeChecked` and `stylisticTypeChecked`,
checks Prettier formatting and that `web/dist` matches a fresh bundle. No
`any`, no type assertions, and `unknown` only where `decode.ts` checks
`JSON.parse`'s output: all server data is decoded field by field.

Tests: `cargo nextest run` (or `cargo test`) for the Rust code, `cd web && bun
test` for the page logic; both are part of `just check`. Unit tests sit next to
each module (`src/<module>_tests.rs`), with timetables written as small tables
(`src/testkit.rs`) and trimmed real answers of the APIs in `testdata/`; property
tests compare the search, profile scan and reachability against brute force on
random timetables. `src/e2e_tests.rs` runs whole searches and the web front end
against local stand-ins for Transitous, EFA and the DB API — no test touches the
network or the user's caches. `just coverage` reports line coverage.

CI (`.github/workflows/ci.yml`) runs the same `just` recipes on every push to
`main` and every pull request, one job each: rustfmt, clippy, tests, rustdoc,
web (TypeScript, ESLint, Prettier, `bun test`, `web/dist` up to date) and
coverage (summary on the run page, lcov as artifact). The Rust toolchain is
pinned in `rust-toolchain.toml`, Bun in the workflow.

Pull requests also pass a mutation gate (`.github/workflows/mutants.yml`),
informed by the diff: a mutant — a small change like `<` → `<=` — in a line the
PR changes that no test notices fails the PR; untouched code doesn't count.
Rust: `just mutants-diff` (cargo-mutants; `just mutants` runs all, about an
hour); a mutant that makes a test hang counts as caught, and equivalent mutants
are excluded with their reason in `.cargo/mutants.toml`. Web: `just
web-mutants-diff` (Stryker on the helper modules, not the DOM scripts; `just
web-mutants` runs all of them); equivalent mutants carry a `// Stryker disable
next-line` comment saying why. Stryker mutates in place (restoring
afterwards), since `data.test.ts` reads `../testdata`. Currently no mutant
survives anywhere, so full runs pass too.
