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
  06:39 → 17:42  11h03  2×  min 42m   06:39 (+1) ICE 378 → 07:15 (+2) Frankfurt (Main) Südbahnhof ‖49m‖ 08:04 (+12) ICE 2558 + ICE 76 → 11:51 Hamburg Hbf ‖42m‖ 12:33 (+5) RJ 384 → 17:42 (+4) København H
  06:39 → 17:42  11h03  2×  min 36m   06:39 (+1) ICE 378 → 08:21 (+10) Fulda Bahnhof ‖48m‖ 09:09 (+5) ICE 886 → 11:57 Hamburg Hbf ‖36m‖ 12:33 (+5) RJ 384 → 17:42 (+4) København H
  …
```

`‖42m‖` is the buffer at a change, `⚠` marks buffers below `--min-transfer`,
`(+5)` live delays. `umstieg --help` lists all options.

`--detail` adds platforms and, for every change, up to three fallbacks if the
next train is missed: connections leaving within 2 h with at most 3 changes,
cut where they catch a train of the plan again ("back on plan"), e.g.

```
    08:04 (+12) Frankfurt (Main) Südbahnhof [5] → 11:52 (+1) Hamburg Hbf [12]   ICE 2558 + ICE 76
      ↳ 48m buffer · if missed:
          12:42 S1 (41618) → 12:54 Altona ‖14m‖ 13:08 RJ 384 (back on plan) · arrive 22:05 (+0m)
          12:52 S1 (41620) → 13:04 Altona ‖4m⚠‖ 13:08 RJ 384 (back on plan) · arrive 22:05 (+0m)
```

### Plot

`--html out.html` writes a self-contained interactive time–distance diagram
(a Marey chart generalised to a network): x is time, y a fixed height per
station with the goal at the bottom. Every stretch is drawn once, however many
connections use it: riding in one colour per train (follow a train across all
connections), waiting as a thin horizontal line (merged per continuous stay;
red below `--min-transfer`), walking dotted. Later departures of the same route
run parallel, faster trains are steeper and overtake slower ones.

Hovering only previews: it highlights every connection passing the point under
the mouse and lists them by group. On a ride that is every connection on that
train between those two stops, wherever it boards or leaves; on a wait, every
connection waiting at that station at that time. Clicking selects: if one
connection passes there it is selected, otherwise (e.g. 63 connections on ICE
378 Mannheim → Frankfurt Süd) they are listed in the side panel, where hovering
previews and clicking selects one (the other listed connections then fade to
30 %);
`↑`/`↓` step through the list, `Esc` clears. The selection gets its changes
labelled and its fallbacks drawn dashed. Hovering a group (first … last train)
highlights it, clicking hides it.

The mouse wheel zooms around the pointer (`Shift`: time only, `Alt`: height
only), dragging pans, double-click or "Reset zoom" shows everything again.
Station labels and time ticks follow the zoom.

`--metric` sets the station height:

| Metric | Height | Equidistant lines |
|---|---|---|
| `remaining-time` (default) | fastest possible remaining ride time, ignoring waits | network-based; riding is never flat, at most 45°, exactly 45° on the fastest train |
| `hyperbolic` | (d_goal − d_start + D) / 2 | hyperbolas, start and goal as foci |
| `axis` | distance left along the start–goal axis (from d_start² − d_goal²) | straight lines perpendicular to the axis |
| `ratio` | d_goal / (d_start + d_goal) | Apollonius circles |

With geographic metrics, riding sideways to the axis looks flat and moving away
from the goal goes up (e.g. København → Malmö on the way to Göteborg).

## How it works

1. **Rough routes** (MOTIS/Transitous): the planner's answers, plus
   origin → hub and hub → destination halves for the stations where those
   answers change trains. They pick the corridor.
2. **Timetable** (DB Timetables API): hourly boards for every station where a
   rough route boards or leaves a train (`--all-stops`: every stop of those
   trains, about three times the requests), plus stations where DB long-distance
   trains call before the next corridor station (this finds Frankfurt Süd).
   Trains are joined across boards by DB's run id, live changes applied.
   MOTIS trains are merged in by number or identical times; DB wins where it
   has data, MOTIS fills the rest (abroad, buses). Stations from different
   feeds are unified by EVA number, name or distance (< 300 m).
3. **Enumeration**: depth-first over that timetable, deepening by number of
   changes, within these limits:
   - departure inside `--window`, travel time ≤ fastest + `--extra-hours`,
   - at most `--max-changes` changes, any transfer time (short ones flagged),
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

   One variant per train sequence is kept, the one with the largest minimum
   buffer. If the effort cap (`--max-nodes`) is hit, the output says up to
   how many changes the list is complete.

Without DB credentials, the DB part of step 2 is skipped and the timetable consists
of the MOTIS trains only.

## Data

MOTIS data comes from [Transitous](https://transitous.org)
([sources](https://transitous.org/sources/)), a volunteer-run instance. One
search makes roughly 10–40 requests, two in parallel, backing off on HTTP 429.
Set `UMSTIEG_CONTACT` for the User-Agent, as the
[usage policy](https://transitous.org/api/) requires. `--api` points at another
MOTIS instance.

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
