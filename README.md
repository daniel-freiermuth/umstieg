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
`(+5)` live delays. `--detail` adds platforms and the earliest arrival if a
change is missed. `umstieg --help` lists all options.

## How it works

1. **Rough routes** (MOTIS/Transitous): the planner's answers, plus
   origin → hub and hub → destination halves for the stations where those
   answers change trains. They pick the corridor.
2. **Timetable** (DB Timetables API): hourly boards for every station where a
   rough route boards or leaves a train, plus stations where DB long-distance
   trains call before the next corridor station (this finds Frankfurt Süd).
   Trains are joined across boards by DB's run id, live changes applied.
   MOTIS trains are merged in by number or identical times; DB wins where it
   has data, MOTIS fills the rest (abroad, buses). Stations from different
   feeds are unified by EVA number, name or distance (< 300 m).
3. **Enumeration**: depth-first over that timetable, deepening by number of
   changes, within these limits:
   - departure inside `--window`, travel time ≤ fastest + `--extra-hours`,
   - at most `--max-changes` changes, any transfer time (short ones flagged),
   - no station twice (also not passing through), every train must bring you
     closer to the destination,
   - no detours: a train is not boarded if the chain already stopped at an
     earlier station where it could have been caught comfortably — including
     leaving the origin later on the same train (`--keep-detours` disables).

   One variant per train sequence is kept, the one with the largest minimum
   buffer. If the effort cap (`--max-nodes`) is hit, the output says up to
   how many changes the list is complete.

Without DB credentials, steps 2's DB part is skipped and the timetable consists
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

- Connections through stations outside the corridor are not found.
- Minimum transfer times per station are not available from these APIs; one
  global `--min-transfer` is used for flagging.
- DB boards cover German stations (and a few abroad); elsewhere times come from
  Transitous.
