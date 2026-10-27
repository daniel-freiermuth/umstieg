//! Every connection within the limits, not just the Pareto-optimal ones.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};

use crate::checked;
use crate::timetable::{Call, Time, Timetable};

/// Element `i` of a per-station (or per-trip) table, or an error naming it:
/// indexes all come from the same timetable, so this only fails on a bug.
fn entry<'v, T>(v: &'v [T], i: usize, what: &'static str) -> Result<&'v T> {
    v.get(i).with_context(|| format!("no {what} {i} in the timetable"))
}

fn entry_mut<'v, T>(v: &'v mut [T], i: usize, what: &'static str) -> Result<&'v mut T> {
    v.get_mut(i).with_context(|| format!("no {what} {i} in the timetable"))
}

pub struct Limits {
    /// Where connections start: the trip's origin, or a change station for fallbacks.
    pub origin: usize,
    pub depart_from: DateTime<Utc>,
    pub depart_until: DateTime<Utc>,
    /// Changes allowed per connection.
    pub max_changes: usize,
    /// Allowed travel time beyond the fastest connection.
    pub extra_time: TimeDelta,
    /// Changes shorter than this are flagged; detour pruning assumes it.
    pub min_transfer: TimeDelta,
    /// Changes live data says are missed by up to this much are listed too:
    /// delays may still shift either way.
    pub missed_by: TimeDelta,
    /// Keep chains that take a train they could have boarded at an earlier stop.
    pub keep_detours: bool,
    /// Only rides that reduce the best-case remaining travel time. Off by
    /// default: riding away can pay off (S-Bahn to Heidelberg for an ECE
    /// that skips Mannheim).
    pub forward_only: bool,
    /// Search effort cap; deepening stops at the first change count exceeding it.
    pub max_nodes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    Ride { trip: usize, board: usize, alight: usize },
    Walk { footpath: usize },
}

#[derive(Debug, Clone)]
pub struct Chain {
    pub steps: Vec<Step>,
}

pub struct Transfer {
    /// Where the previous train arrives.
    pub station: usize,
    pub departure: DateTime<Utc>,
    pub walk: TimeDelta,
    /// Waiting time left after walking.
    pub slack: TimeDelta,
}

impl Chain {
    pub fn rides(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.steps.iter().filter_map(|s| match *s {
            Step::Ride { trip, board, alight } => Some((trip, board, alight)),
            Step::Walk { .. } => None,
        })
    }

    pub fn changes(&self) -> usize {
        // A chain without rides (none is ever built) has no changes either.
        self.rides().count().saturating_sub(1)
    }

    pub fn departure(&self, tt: &Timetable) -> Result<DateTime<Utc>> {
        let (trip, board, _) = self.rides().next().context("connection without a train")?;
        tt.call(trip, board)?.departure_time().with_context(|| format!("trip {trip} doesn't depart at call {board}"))
    }

    /// Arrival at the destination: the last train's, plus a walk after it.
    pub fn arrival(&self, tt: &Timetable) -> Result<DateTime<Utc>> {
        let mut walk = TimeDelta::zero();
        for step in self.steps.iter().rev() {
            match *step {
                Step::Walk { footpath } => walk = checked::sum(walk, tt.footpath(footpath)?.duration),
                Step::Ride { .. } => break,
            }
        }
        let (trip, _, alight) = self.rides().last().context("connection without a train")?;
        let arrival = tt
            .call(trip, alight)?
            .arrival_time()
            .with_context(|| format!("trip {trip} doesn't arrive at call {alight}"))?;
        checked::later(arrival, walk, "arrival after the last walk")
    }

    pub fn transfers(&self, tt: &Timetable) -> Result<Vec<Transfer>> {
        let mut out = Vec::new();
        let mut previous: Option<(usize, DateTime<Utc>)> = None;
        let mut walk = TimeDelta::zero();
        for step in &self.steps {
            match *step {
                Step::Walk { footpath } => walk = checked::sum(walk, tt.footpath(footpath)?.duration),
                Step::Ride { trip, board, alight } => {
                    let departure = tt
                        .call(trip, board)?
                        .departure_time()
                        .with_context(|| format!("trip {trip} doesn't depart at call {board}"))?;
                    if let Some((station, arrival)) = previous {
                        let slack = checked::difference(departure.signed_duration_since(arrival), walk);
                        out.push(Transfer { station, departure, walk, slack });
                    }
                    let leave = tt.call(trip, alight)?;
                    let arrival =
                        leave.arrival_time().with_context(|| format!("trip {trip} doesn't arrive at call {alight}"))?;
                    previous = Some((leave.station, arrival));
                    walk = TimeDelta::zero();
                }
            }
        }
        Ok(out)
    }

    pub fn min_slack(&self, tt: &Timetable) -> Result<Option<TimeDelta>> {
        Ok(self.transfers(tt)?.iter().map(|t| t.slack).min())
    }
}

pub struct Enumeration {
    pub chains: Vec<Chain>,
    /// Shortest travel time found; the duration limit is based on it. If
    /// only missed connections (within `missed_by`) exist, the fastest of those.
    pub fastest: Option<TimeDelta>,
    /// Highest number of changes for which the list is complete; below
    /// `max_changes` when the effort cap was hit.
    pub complete_up_to: Option<usize>,
    pub nodes: usize,
}

/// Keeps the smaller of `slot` and `v` in `slot`; whether `v` was smaller.
fn improve<T: Ord + Copy>(slot: &mut Option<T>, v: T) -> bool {
    let better = slot.is_none_or(|e| v < e);
    if better {
        *slot = Some(v);
    }
    better
}

/// Earliest arrival at every station when leaving `start` at `t0` on a train
/// departing there by `latest`, with changes of any length, even missed by
/// up to `missed_by`. As in the enumeration, walks only follow a ride: none
/// from `start`, never two in a row.
pub fn earliest_arrival(
    tt: &Timetable,
    start: usize,
    t0: DateTime<Utc>,
    latest: DateTime<Utc>,
    missed_by: TimeDelta,
) -> Result<Vec<Option<DateTime<Utc>>>> {
    let n = tt.stations.len();
    // Arrivals by train, and by a walk after one.
    let mut rode: Vec<Option<DateTime<Utc>>> = vec![None; n];
    let mut walked: Vec<Option<DateTime<Utc>>> = vec![None; n];
    loop {
        let mut changed = false;
        for trip in &tt.trips {
            let mut aboard = false;
            for call in trip.calls.iter().filter(|c| !c.cancelled) {
                if aboard {
                    if let Some(a) = call.arrival_time() {
                        changed |= improve(entry_mut(&mut rode, call.station, "station")?, a);
                    }
                } else if let Some(d) = call.departure_time() {
                    aboard = if call.station == start {
                        (t0..=latest).contains(&d)
                    } else {
                        let (r, w) = (entry(&rode, call.station, "station")?, entry(&walked, call.station, "station")?);
                        // Before the earliest time there is: any train is catchable.
                        r.iter().chain(w).any(|&e| checked::earlier_or_min(e, missed_by) <= d)
                    };
                }
            }
        }
        for fp in tt.footpaths.iter().filter(|fp| fp.from != start) {
            // A walk ending after the latest time there is reaches nothing.
            if let Some(e) = *entry(&rode, fp.from, "station")?
                && let Some(t) = e.checked_add_signed(fp.duration)
            {
                changed |= improve(entry_mut(&mut walked, fp.to, "station")?, t);
            }
        }
        if !changed {
            return Ok(rode
                .into_iter()
                .zip(walked)
                .enumerate()
                .map(|(s, (r, w))| if s == start { Some(t0) } else { r.into_iter().chain(w).min() })
                .collect());
        }
    }
}

/// Lower bound on the remaining travel time from each station, ignoring waits:
/// fastest ride per segment over the whole timetable, summed along the best path.
/// `ride` is a segment's time from its departure and arrival. Cancelled stops
/// are skipped (ICE 857 "Wuppertal 09:41 → Hagen 09:41"); a stop with only
/// one time is passed at that time.
fn lower_bounds(tt: &Timetable, ride: impl Fn(Time, Time) -> TimeDelta) -> Result<Vec<Option<TimeDelta>>> {
    let n = tt.stations.len();
    let mut reverse: Vec<Vec<(usize, TimeDelta)>> = vec![Vec::new(); n];
    for trip in &tt.trips {
        let mut previous: Option<(usize, Time)> = None;
        for call in trip.calls.iter().filter(|c| !c.cancelled) {
            if let (Some((from, departure)), Some(arrival)) = (previous, call.arrival.or(call.departure)) {
                let edge = (from, ride(departure, arrival).max(TimeDelta::zero()));
                entry_mut(&mut reverse, call.station, "station")?.push(edge);
            }
            previous = call.departure.or(call.arrival).map(|d| (call.station, d));
        }
    }
    for fp in &tt.footpaths {
        entry_mut(&mut reverse, fp.to, "station")?.push((fp.from, fp.duration));
    }
    let mut best: Vec<Option<TimeDelta>> = vec![None; n];
    let mut heap = BinaryHeap::new();
    *entry_mut(&mut best, tt.destination, "destination station")? = Some(TimeDelta::zero());
    heap.push(Reverse((TimeDelta::zero(), tt.destination)));
    while let Some(Reverse((d, s))) = heap.pop() {
        if entry(&best, s, "station")?.is_some_and(|b| d > b) {
            continue;
        }
        for &(prev, w) in entry(&reverse, s, "station")? {
            // Saturating: a bound beyond the longest duration there is prunes all the same.
            let nd = checked::sum(d, w);
            if improve(entry_mut(&mut best, prev, "station")?, nd) {
                heap.push(Reverse((nd, prev)));
            }
        }
    }
    Ok(best)
}

/// Duration of the fastest real journey from each station to the destination,
/// waits and walks included, over all departures in the timetable (profile
/// connection scan). Any change time is allowed, as in the enumeration.
pub fn fastest_journeys(tt: &Timetable) -> Result<Vec<Option<TimeDelta>>> {
    struct Ride {
        from: usize,
        to: usize,
        departure: DateTime<Utc>,
        arrival: DateTime<Utc>,
        trip: usize,
        /// Positions of `from` and `to` in the trip.
        seq: (usize, usize),
        /// Whether the train can be boarded at `from`, not just passed.
        boardable: bool,
    }
    let n = tt.stations.len();
    let mut rides = Vec::new();
    for (trip, t) in tt.trips.iter().enumerate() {
        let mut previous: Option<(usize, DateTime<Utc>, bool, usize)> = None;
        for (seq, call) in t.calls.iter().enumerate().filter(|(_, c)| !c.cancelled) {
            if let (Some((from, departure, boardable, from_seq)), Some(arrival)) = (previous, call.arrival_time()) {
                rides.push(Ride { from, to: call.station, departure, arrival, trip, seq: (from_seq, seq), boardable });
            }
            // A stop without a departure time is passed, not boarded at.
            previous = match (call.departure_time(), call.arrival_time()) {
                (Some(d), _) => Some((call.station, d, true, seq)),
                (None, Some(a)) => Some((call.station, a, false, seq)),
                (None, None) => None,
            };
        }
    }
    // Latest first; a trip's later ride before its earlier one at equal times.
    rides.sort_by_key(|r| Reverse((r.departure, r.arrival, r.seq)));
    let mut walks: Vec<Vec<(usize, TimeDelta)>> = vec![Vec::new(); n];
    for fp in &tt.footpaths {
        entry_mut(&mut walks, fp.from, "station")?.push((fp.to, fp.duration));
    }

    // Per station, (departure, arrival at goal) with both falling: the earliest
    // arrival departing at or after t is the last entry departing at or after t.
    let mut profiles: Vec<Vec<(DateTime<Utc>, DateTime<Utc>)>> = vec![Vec::new(); n];
    let reach = |profiles: &[Vec<(DateTime<Utc>, DateTime<Utc>)>], station: usize, t: DateTime<Utc>| {
        let p = entry(profiles, station, "station")?;
        Ok::<_, anyhow::Error>(p.partition_point(|&(d, _)| d >= t).checked_sub(1).and_then(|k| p.get(k)).map(|e| e.1))
    };
    // Per trip and stop: earliest arrival at the goal when on the train leaving there.
    let mut staying: Vec<Vec<Option<DateTime<Utc>>>> = tt.trips.iter().map(|t| vec![None; t.calls.len()]).collect();
    let mut best: Vec<Option<TimeDelta>> = vec![None; n];
    *entry_mut(&mut best, tt.destination, "destination station")? = Some(TimeDelta::zero());
    // Whether the ride improved anything.
    let mut scan = |r: &Ride| {
        let stops = entry_mut(&mut staying, r.trip, "trip")?;
        let mut goal = *entry(stops, r.seq.1, "call")?;
        let mut consider = |t: Option<DateTime<Utc>>| {
            if let Some(t) = t {
                goal = Some(goal.map_or(t, |g: DateTime<Utc>| g.min(t)));
            }
        };
        if r.to == tt.destination {
            consider(Some(r.arrival));
        }
        consider(reach(&profiles, r.to, r.arrival)?);
        for &(next, walk) in entry(&walks, r.to, "station")? {
            // A walk ending after the latest time there is reaches nothing.
            let Some(walked) = r.arrival.checked_add_signed(walk) else { continue };
            if next == tt.destination {
                consider(Some(walked));
            }
            consider(reach(&profiles, next, walked)?);
        }
        let Some(goal) = goal else { return Ok(false) };
        let changed = improve(entry_mut(stops, r.seq.0, "call")?, goal);
        if !r.boardable {
            return Ok(changed);
        }
        let p = entry_mut(&mut profiles, r.from, "station")?;
        match p.last_mut() {
            Some(last) if last.1 <= goal => return Ok(changed),
            Some(last) if last.0 == r.departure => last.1 = goal,
            _ => p.push((r.departure, goal)),
        }
        let duration = goal.signed_duration_since(r.departure);
        improve(entry_mut(&mut best, r.from, "station")?, duration);
        Ok::<_, anyhow::Error>(true)
    };
    for same_time in rides.chunk_by(|a, b| a.departure == b.departure) {
        // Rides taking no time (squeezed live times) can lead onto each other
        // in any order: repeat until settled.
        let instant = same_time.iter().any(|r| r.arrival == r.departure);
        loop {
            let mut changed = false;
            for r in same_time {
                changed |= scan(r)?;
            }
            if !(changed && instant) {
                break;
            }
        }
    }
    // Walking to a station and leaving from there by train (one walk, as
    // between trains: never two in a row).
    let by_train = best.clone();
    for fp in &tt.footpaths {
        // Saturating: a duration beyond the longest there is is never the fastest.
        if let Some(after) = entry(&by_train, fp.to, "station")?.map(|b| checked::sum(b, fp.duration)) {
            improve(entry_mut(&mut best, fp.from, "station")?, after);
        }
    }
    Ok(best)
}

/// Per-timetable indexes shared by all searches on it (main and fallbacks).
pub struct Network<'a> {
    pub tt: &'a Timetable,
    /// Per station: (trip, call, departure) sorted by departure.
    departures: Vec<Vec<(usize, usize, DateTime<Utc>)>>,
    walks: Vec<Vec<usize>>,
    /// Best-case remaining travel time from each station, by planned ride
    /// times: live times of delayed trains are often squeezed to zero
    /// (ICE 1091 Ludwigshafen 05:15 → Mannheim 05:15 at +10/+6).
    pub lower_bound: Vec<Option<TimeDelta>>,
    /// Like `lower_bound`, but never above a live ride time either (a delayed
    /// train catching up), so pruning by it keeps every connection within the limits.
    bound: Vec<Option<TimeDelta>>,
}

impl<'a> Network<'a> {
    /// Fails if the timetable refers to a station it doesn't have.
    pub fn new(tt: &'a Timetable) -> Result<Self> {
        let n = tt.stations.len();
        let mut departures: Vec<Vec<(usize, usize, DateTime<Utc>)>> = vec![Vec::new(); n];
        for (t, trip) in tt.trips.iter().enumerate() {
            for (c, call) in trip.calls.iter().enumerate().take(trip.calls.len().saturating_sub(1)) {
                if let Some(departure) = call.departure_time().filter(|_| !call.cancelled) {
                    entry_mut(&mut departures, call.station, "station")?.push((t, c, departure));
                }
            }
        }
        for list in &mut departures {
            list.sort_by_key(|&(_, _, departure)| departure);
        }
        let mut walks = vec![Vec::new(); n];
        for (i, fp) in tt.footpaths.iter().enumerate() {
            entry_mut(&mut walks, fp.from, "station")?.push(i);
        }
        entry(&tt.stations, tt.origin, "origin station")?;
        Ok(Self {
            tt,
            departures,
            walks,
            lower_bound: lower_bounds(tt, |d, a| a.planned.signed_duration_since(d.planned))?,
            bound: lower_bounds(tt, |d, a| {
                a.planned.signed_duration_since(d.planned).min(a.actual.signed_duration_since(d.actual))
            })?,
        })
    }
}

/// A station the chain stopped at.
struct Presence {
    station: usize,
    /// Arrival; `None` at the origin.
    arrival: Option<DateTime<Utc>>,
    /// From when trains there count as catchable (the origin: the chain's first departure).
    since: DateTime<Utc>,
    /// Buffer of the change made here, once the next train is chosen.
    slack_out: Option<TimeDelta>,
}

struct Search<'a> {
    tt: &'a Timetable,
    limits: &'a Limits,
    departures: &'a [Vec<(usize, usize, DateTime<Utc>)>],
    walks: &'a [Vec<usize>],
    lower_bound: &'a [Option<TimeDelta>],
    bound: &'a [Option<TimeDelta>],
    max_duration: TimeDelta,
    visited: Vec<bool>,
    used: Vec<bool>,
    steps: Vec<Step>,
    rides: usize,
    /// Rides allowed in the current deepening round.
    max_rides: usize,
    /// Stations where the chain stopped, and from when another train could be caught there.
    presence: Vec<Presence>,
    /// Every connection found once; deepening by changes revisits shorter ones.
    found: HashSet<Vec<Step>>,
    nodes: usize,
    truncated: bool,
    report: &'a dyn Fn(Progress),
    /// When to report next.
    report_at: Instant,
}

/// How a running enumeration is getting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Changes allowed in the current deepening round.
    pub changes: usize,
    /// States explored so far.
    pub nodes: usize,
    /// Connections found so far.
    pub found: usize,
}

/// How often a running search reports, about (tests: at every chance).
pub const REPORT_EVERY: Duration = if cfg!(test) { Duration::ZERO } else { Duration::from_secs(1) };
/// The clock is read every this many states (tests: every state).
const REPORT_STRIDE: usize = if cfg!(test) { 1 } else { 1 << 16 };

/// The time `REPORT_EVERY` from now (now, should that overflow).
pub fn next_report() -> Instant {
    let now = Instant::now();
    now.checked_add(REPORT_EVERY).unwrap_or(now)
}

pub fn enumerate(net: &Network<'_>, limits: &Limits) -> Result<Enumeration> {
    enumerate_reporting(net, limits, &|_| {})
}

/// [`enumerate`], telling `report` about every `REPORT_EVERY` how it is getting on.
pub fn enumerate_reporting(net: &Network<'_>, limits: &Limits, report: &dyn Fn(Progress)) -> Result<Enumeration> {
    let tt = net.tt;
    let starts = entry(&net.departures, limits.origin, "origin station")?;
    let fastest_with = |missed_by| -> Result<Option<TimeDelta>> {
        let mut fastest: Option<TimeDelta> = None;
        for &(_, _, d) in starts.iter().filter(|&&(_, _, d)| (limits.depart_from..=limits.depart_until).contains(&d)) {
            let arrivals = earliest_arrival(tt, limits.origin, d, limits.depart_until, missed_by)?;
            if let Some(a) = *entry(&arrivals, tt.destination, "destination station")? {
                let duration = a.signed_duration_since(d);
                fastest = Some(fastest.map_or(duration, |f| f.min(duration)));
            }
        }
        Ok(fastest)
    };
    // Connections just missed are listed too, also when nothing else works.
    let fastest = match fastest_with(TimeDelta::zero())? {
        None if limits.missed_by > TimeDelta::zero() => fastest_with(limits.missed_by)?,
        fastest => fastest,
    };
    let Some(fastest_duration) = fastest else {
        return Ok(Enumeration {
            chains: Vec::new(),
            fastest: None,
            complete_up_to: Some(limits.max_changes),
            nodes: 0,
        });
    };

    let mut search = Search {
        tt,
        limits,
        departures: &net.departures,
        walks: &net.walks,
        lower_bound: &net.lower_bound,
        bound: &net.bound,
        // Saturating: a limit beyond the longest duration there is limits nothing.
        max_duration: checked::sum(fastest_duration, limits.extra_time),
        visited: vec![false; tt.stations.len()],
        used: vec![false; tt.trips.len()],
        steps: Vec::new(),
        rides: 0,
        max_rides: 0,
        presence: Vec::new(),
        found: HashSet::new(),
        nodes: 0,
        truncated: false,
        report,
        report_at: next_report(),
    };
    *entry_mut(&mut search.visited, limits.origin, "origin station")? = true;
    // Deepen by changes so that hitting the effort cap still leaves complete
    // lists for few changes instead of an arbitrary depth-first subset.
    let mut complete_up_to = None;
    for changes in 0..=limits.max_changes {
        // Saturating: usize::MAX rides is no limit either.
        search.max_rides = changes.saturating_add(1);
        search.explore(limits.origin, limits.depart_from, None, false)?;
        if search.truncated {
            break;
        }
        complete_up_to = Some(changes);
    }

    let chains = search.found.into_iter().map(|steps| Chain { steps }).collect();
    Ok(Enumeration { chains, fastest, complete_up_to, nodes: search.nodes })
}

impl Search<'_> {
    /// Whether to report progress now: the clock is read only now and then.
    fn report_due(&self) -> bool {
        self.nodes.is_multiple_of(REPORT_STRIDE) && Instant::now() >= self.report_at
    }

    /// At `station`, ready to leave at `ready`. `deadline` is set once the first train is chosen.
    #[expect(
        clippy::too_many_lines,
        reason = "the search step: walks, then trains and their stops, sharing the backtracking state"
    )]
    fn explore(
        &mut self,
        station: usize,
        ready: DateTime<Utc>,
        deadline: Option<DateTime<Utc>>,
        walked: bool,
    ) -> Result<()> {
        // Saturating: past the cap, the count only has to stay above it.
        self.nodes = self.nodes.saturating_add(1);
        if self.report_due() {
            self.report_at = next_report();
            (self.report)(Progress {
                changes: self.max_rides.saturating_sub(1),
                nodes: self.nodes,
                found: self.found.len(),
            });
        }
        if self.nodes > self.limits.max_nodes {
            self.truncated = true;
            return Ok(());
        }
        if station == self.tt.destination {
            self.record();
            return Ok(());
        }
        let (tt, departures, walks) = (self.tt, self.departures, self.walks);

        if let Some(deadline) = deadline.filter(|_| !walked) {
            for &footpath in entry(walks, station, "station")? {
                let fp = tt.footpath(footpath)?;
                // A walk ending after the latest time there is misses any deadline.
                let Some(arrive) = ready.checked_add_signed(fp.duration) else { continue };
                let to = fp.to;
                if *entry(&self.visited, to, "station")?
                    || arrive > deadline
                    || !self.reachable(to, arrive, deadline)?
                {
                    continue;
                }
                *entry_mut(&mut self.visited, to, "station")? = true;
                self.presence.push(Presence { station: to, arrival: Some(arrive), since: arrive, slack_out: None });
                self.steps.push(Step::Walk { footpath });
                self.explore(to, arrive, Some(deadline), true)?;
                self.steps.pop();
                self.presence.pop();
                *entry_mut(&mut self.visited, to, "station")? = false;
            }
        }
        if self.rides >= self.max_rides {
            return Ok(());
        }

        // Trains may leave up to `missed_by` before the chain gets here; before
        // the earliest time there is, that's any train.
        let catchable_from = checked::earlier_or_min(ready, self.limits.missed_by);
        for &(trip, board, departure) in entry(departures, station, "station")? {
            if *entry(&self.used, trip, "trip")? {
                continue;
            }
            let calls = &tt.trip(trip)?.calls;
            let deadline = match deadline {
                Some(d) if departure > d => break,
                Some(d) => {
                    if departure < catchable_from {
                        continue;
                    }
                    d
                }
                None => {
                    if departure < self.limits.depart_from {
                        continue;
                    }
                    if departure > self.limits.depart_until {
                        break;
                    }
                    // Saturating: a deadline after the latest time there is excludes nothing.
                    checked::later_or_max(departure, self.max_duration)
                }
            };
            let earlier_calls = calls.get(..board).with_context(|| format!("trip {trip} has no call {board}"))?;
            if !self.limits.keep_detours && self.is_detour(earlier_calls, departure) {
                continue;
            }

            let first = self.rides == 0;
            match self.presence.last_mut() {
                // The change made here, for detour comparisons further on.
                Some(here) if !first => here.slack_out = here.arrival.map(|a| departure.signed_duration_since(a)),
                // Leaving later on the same train from the origin is never worse.
                _ => self.presence.push(Presence { station, arrival: None, since: departure, slack_out: None }),
            }
            *entry_mut(&mut self.used, trip, "trip")? = true;
            // Below `max_rides` here, so this never saturates.
            self.rides = self.rides.saturating_add(1);
            let mut passed = Vec::new();
            for (alight, call) in calls.iter().enumerate().skip(board.saturating_add(1)) {
                let seen = entry_mut(&mut self.visited, call.station, "station")?;
                if *seen {
                    break;
                }
                let Some(arrival) = call.arrival_time() else { break };
                // Arriving before departing means merged data disagrees; don't ride on it.
                if arrival > deadline || arrival < departure {
                    break;
                }
                *seen = true;
                passed.push(call.station);
                let worth_stopping = call.station == tt.destination
                    || !entry(departures, call.station, "station")?.is_empty()
                    || !entry(walks, call.station, "station")?.is_empty();
                let progress = !self.limits.forward_only
                    || entry(self.lower_bound, call.station, "station")? < entry(self.lower_bound, station, "station")?;
                if !call.cancelled && worth_stopping && progress && self.reachable(call.station, arrival, deadline)? {
                    let at =
                        Presence { station: call.station, arrival: Some(arrival), since: arrival, slack_out: None };
                    self.presence.push(at);
                    self.steps.push(Step::Ride { trip, board, alight });
                    self.explore(call.station, arrival, Some(deadline), false)?;
                    self.steps.pop();
                    self.presence.pop();
                }
                if self.truncated {
                    break;
                }
            }
            for s in passed {
                *entry_mut(&mut self.visited, s, "station")? = false;
            }
            // Incremented above.
            self.rides = self.rides.saturating_sub(1);
            *entry_mut(&mut self.used, trip, "trip")? = false;
            if first {
                self.presence.pop();
            } else if let Some(here) = self.presence.last_mut() {
                here.slack_out = None;
            }
            if self.truncated {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Boarding this train where the chain stands (the last presence) is a
    /// detour if it already called at a station the chain stopped at earlier,
    /// catchable there comfortably and with at least the buffer the detour's
    /// changes leave. Detours that raise the worst buffer (S1 to Altona
    /// instead of a short change at Hamburg Hbf) stay. At the origin, any
    /// later departure of the train counts.
    fn is_detour(&self, earlier_calls: &[Call], departure: DateTime<Utc>) -> bool {
        let Some((here, before)) = self.presence.split_last() else { return false };
        // Worst buffer of the detour from an earlier stop up to boarding here.
        let mut worst = here.arrival.map_or(TimeDelta::MAX, |a| departure.signed_duration_since(a));
        for p in before.iter().rev() {
            let caught = earlier_calls.iter().find(|c| c.station == p.station).and_then(Call::departure_time);
            if let Some(d) = caught.filter(|&d| d >= p.since) {
                match p.arrival {
                    None => return true,
                    Some(arrival) => {
                        let direct = d.signed_duration_since(arrival);
                        if direct >= self.limits.min_transfer
                            && direct >= worst.min(p.slack_out.unwrap_or(TimeDelta::MAX))
                        {
                            return true;
                        }
                    }
                }
            }
            worst = worst.min(p.slack_out.unwrap_or(TimeDelta::MAX));
        }
        false
    }

    /// Whether the destination can still be reached by `deadline` from
    /// `station` at `at`. Every train still to board may leave up to
    /// `missed_by` before the chain gets there.
    fn reachable(&self, station: usize, at: DateTime<Utc>, deadline: DateTime<Utc>) -> Result<bool> {
        // `rides` never exceeds `max_rides`.
        let boardings = i32::try_from(self.max_rides.saturating_sub(self.rides)).unwrap_or(i32::MAX);
        let early = self.limits.missed_by.checked_mul(boardings).unwrap_or(TimeDelta::MAX);
        // Past the latest time there is: no deadline, or never there in time.
        Ok(entry(self.bound, station, "station")?.is_some_and(|lb| {
            deadline
                .checked_add_signed(early)
                .is_none_or(|d| at.checked_add_signed(lb).is_some_and(|earliest| earliest <= d))
        }))
    }

    /// Every variant counts, also ones riding the same trains but changing
    /// elsewhere (Ludwigshafen Mitte instead of Ludwigshafen Hbf).
    fn record(&mut self) {
        self.found.insert(self.steps.clone());
    }
}

/// Connections sharing first and last train.
pub struct Group {
    pub first: usize,
    pub last: usize,
    pub chains: Vec<Chain>,
}

/// Groups by (first train, last train), each sorted feasible first, then by
/// arrival, changes, buffer; groups ordered by their best connection.
pub fn group(tt: &Timetable, chains: Vec<Chain>) -> Result<Vec<Group>> {
    type Key = (bool, DateTime<Utc>, usize, Reverse<TimeDelta>);
    let key = |c: &Chain| -> Result<Key> {
        let slack = c.min_slack(tt)?.unwrap_or(TimeDelta::MAX);
        Ok((slack < TimeDelta::zero(), c.arrival(tt)?, c.changes(), Reverse(slack)))
    };
    let mut groups: HashMap<(usize, usize), Vec<(Key, Chain)>> = HashMap::new();
    for chain in chains {
        let first = chain.rides().next().context("connection without a train")?.0;
        let last = chain.rides().last().map_or(first, |(trip, _, _)| trip);
        groups.entry((first, last)).or_default().push((key(&chain)?, chain));
    }
    let mut out = groups
        .into_iter()
        .map(|((first, last), mut chains)| {
            chains.sort_by_key(|&(key, _)| key);
            let (best, chain) = chains.first().context("empty group")?;
            let order = (*best, chain.departure(tt)?);
            Ok((order, Group { first, last, chains: chains.into_iter().map(|(_, c)| c).collect() }))
        })
        .collect::<Result<Vec<_>>>()?;
    out.sort_by_key(|&(order, _)| order);
    Ok(out.into_iter().map(|(_, g)| g).collect())
}

#[cfg(test)]
#[path = "enumerate_tests.rs"]
mod tests;
