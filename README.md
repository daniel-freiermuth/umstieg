# umstieg

Finds public transport connections that journey planners hide, and shows how
robust each one is.

Planners keep only trips that are best by departure, arrival and number of
changes. A trip that waits longer at a change for the same final train is
dropped, even though that wait is a buffer against delays. `umstieg`:

1. asks the planner for origin → destination,
2. takes the transfer stations of those answers (plus `--via`) as hubs,
3. plans origin → hub and hub → destination separately and combines them,
4. keeps the Pareto set over departure, arrival, changes and transfer buffer
   (buffers above `--enough-buffer` count as equal),
5. for every change, looks up the earliest arrival if you miss it.

```
$ umstieg "Mannheim Hbf" "København H" --at "2026-10-24 06:30"
06:39 → 17:38  10h59  2 change(s), min buffer 31m   ★ hidden by planner, found via Hamburg Hbf
  06:39 (+9)  Mannheim, Hauptbahnhof [2]  →  08:21 (+10)  Fulda Bahnhof [7]   ICE 378 → S+U Berlin Hauptbahnhof
        ↳ 41m buffer · if missed: arrive 19:38 (+2h00), next: ICE 882 11:04 from Fulda Bahnhof
  09:04  Fulda Bahnhof [6]  →  11:57  Hamburg Hbf [11]   ICE 886 → Hamburg Hbf
        ↳ 31m buffer · if missed: arrive 17:38 (+0m), next: S1 12:52 from Hamburg Hbf
  12:28  Hamburg Hbf  →  17:38  København H   RJ → København H
```

`umstieg --help` lists all options.

## Data

Routing and live data come from [Transitous](https://transitous.org)
([sources](https://transitous.org/sources/)), a volunteer-run MOTIS instance.
One search makes roughly 10–30 requests; requests are limited to two in
parallel and back off on HTTP 429. Set `UMSTIEG_CONTACT` to your contact for the
User-Agent, as the [usage policy](https://transitous.org/api/) requires. For
heavier use, run your own MOTIS and point `--api` at it.

Transitous can lag DB's timetable (e.g. ICE 378 via Frankfurt Süd instead of
Hbf) and misses short-term changes. With credentials for the DB
[Timetables API](https://developers.deutschebahn.com) (free plan, subscribe
your application to it), `umstieg` checks every train of the shown journeys at
its boarding and alighting station:

- confirmed trains take DB's times, live delays and platforms (`DB ✓`),
- trains that are cancelled, don't call there, or whose change breaks under
  DB's times are listed under "Dropped after DB check", and the next-best
  journeys are checked instead,
- stations or products DB has no board for (abroad, S-Bahn platforms with their
  own station, buses) stay unchecked.

Credentials come from `DB_CLIENT_ID`/`DB_API_KEY` or
`~/.config/umstieg/db.env` (`KEY=value` lines). Requests stay under the free
plan's 60 per minute. `--no-db` skips the check. DB's unofficial app APIs
(`db-vendo-client`) answered `OPS_BLOCKED` when tested.
