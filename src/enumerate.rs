//! Every connection within the limits, not just the Pareto-optimal ones.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};

use crate::checked;
use crate::timetable::{Call, CallIdx, FootpathIdx, PerStation, PerTrip, StationIdx, Time, Timetable, TripIdx};

pub struct Limits {
    /// Where connections start: the trip's origin, or a change station for fallbacks.
    pub origin: StationIdx,
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
    Ride { trip: TripIdx, board: CallIdx, alight: CallIdx },
    Walk { footpath: FootpathIdx },
}

/// One train of a chain: the trip and the calls where it is boarded and left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ride {
    pub trip: TripIdx,
    pub board: CallIdx,
    pub alight: CallIdx,
}

#[derive(Debug, Clone)]
pub struct Chain {
    pub steps: Vec<Step>,
}

pub struct Transfer {
    /// Where the previous train arrives.
    pub station: StationIdx,
    pub departure: DateTime<Utc>,
    pub walk: TimeDelta,
    /// Waiting time left after walking.
    pub slack: TimeDelta,
}

impl Transfer {
    /// When the traveller is there for the next train: the arrival plus the walk.
    pub fn ready(&self) -> DateTime<Utc> {
        checked::earlier_or_min(self.departure, self.slack)
    }

    /// When the previous train arrives, where the walk (if any) starts.
    pub fn arrival(&self) -> DateTime<Utc> {
        checked::earlier_or_min(self.ready(), self.walk)
    }
}

impl Chain {
    pub fn rides(&self) -> impl Iterator<Item = Ride> + '_ {
        self.steps.iter().filter_map(|s| match *s {
            Step::Ride { trip, board, alight } => Some(Ride { trip, board, alight }),
            Step::Walk { .. } => None,
        })
    }

    pub fn changes(&self) -> usize {
        // A chain without rides (none is ever built) has no changes either.
        self.rides().count().saturating_sub(1)
    }

    /// When the connection leaves the origin: the first train's departure,
    /// less a walk to it (Göteborg Central to the bus terminal next door).
    pub fn departure(&self, tt: &Timetable) -> Result<DateTime<Utc>> {
        let mut walk = TimeDelta::zero();
        for step in &self.steps {
            match *step {
                Step::Walk { footpath } => walk = checked::sum(walk, tt.footpath(footpath)?.duration),
                Step::Ride { trip, board, .. } => {
                    let departure = tt
                        .call(trip, board)?
                        .departure_time()
                        .with_context(|| format!("trip {} doesn't depart at call {}", trip.0, board.0))?;
                    return Ok(checked::earlier_or_min(departure, walk));
                }
            }
        }
        anyhow::bail!("connection without a train")
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
        let Ride { trip, alight, .. } = self.rides().last().context("connection without a train")?;
        let arrival = tt
            .call(trip, alight)?
            .arrival_time()
            .with_context(|| format!("trip {} doesn't arrive at call {}", trip.0, alight.0))?;
        checked::later(arrival, walk, "arrival after the last walk")
    }

    /// From leaving the origin to arriving.
    fn duration(&self, tt: &Timetable) -> Result<TimeDelta> {
        Ok(self.arrival(tt)?.signed_duration_since(self.departure(tt)?))
    }

    pub fn transfers(&self, tt: &Timetable) -> Result<Vec<Transfer>> {
        let mut out = Vec::new();
        let mut previous: Option<(StationIdx, DateTime<Utc>)> = None;
        let mut walk = TimeDelta::zero();
        for step in &self.steps {
            match *step {
                Step::Walk { footpath } => walk = checked::sum(walk, tt.footpath(footpath)?.duration),
                Step::Ride { trip, board, alight } => {
                    let departure = tt
                        .call(trip, board)?
                        .departure_time()
                        .with_context(|| format!("trip {} doesn't depart at call {}", trip.0, board.0))?;
                    if let Some((station, arrival)) = previous {
                        let slack = checked::difference(departure.signed_duration_since(arrival), walk);
                        out.push(Transfer { station, departure, walk, slack });
                    }
                    let leave = tt.call(trip, alight)?;
                    let arrival = leave
                        .arrival_time()
                        .with_context(|| format!("trip {} doesn't arrive at call {}", trip.0, alight.0))?;
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
    /// Shortest travel time of a connection the search can list with any
    /// number of changes; the duration limit is based on it. If only missed
    /// connections (within `missed_by`) exist, the fastest of those. Where the
    /// effort cap cut short the search for it, a lower bound (and
    /// `complete_up_to` is `None`).
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
/// up to `missed_by`. Walks only follow a ride (none from `start`: a walk
/// from the origin is a separate start), never two in a row. Nothing leaves
/// the destination, as no connection does: with missed changes going back
/// in time, coming back to it could otherwise seem earlier.
pub fn earliest_arrival(
    tt: &Timetable,
    start: StationIdx,
    t0: DateTime<Utc>,
    latest: DateTime<Utc>,
    missed_by: TimeDelta,
) -> Result<PerStation<Option<DateTime<Utc>>>> {
    // Arrivals by train, and by a walk after one.
    let mut rode: PerStation<Option<DateTime<Utc>>> = tt.per_station(None);
    let mut walked: PerStation<Option<DateTime<Utc>>> = tt.per_station(None);
    loop {
        let mut changed = false;
        for trip in &tt.trips {
            let mut aboard = false;
            for call in &trip.calls {
                if aboard {
                    if !call.cancelled
                        && let Some(a) = call.arrival_time()
                    {
                        changed |= improve(rode.get_mut(call.station)?, a);
                    }
                    // The search stops there; passing it, even where the stop is cancelled, rules it out.
                    if call.station == tt.destination {
                        break;
                    }
                } else if !call.cancelled
                    && let Some(d) = call.departure_time()
                {
                    aboard = if call.station == start {
                        (t0..=latest).contains(&d)
                    } else if call.station == tt.destination {
                        // The search stops there.
                        false
                    } else {
                        let (r, w) = (rode.get(call.station)?, walked.get(call.station)?);
                        // Before the earliest time there is: any train is catchable.
                        r.iter().chain(w).any(|&e| checked::earlier_or_min(e, missed_by) <= d)
                    };
                }
            }
        }
        for fp in tt.footpaths.iter().filter(|fp| fp.from != start && fp.from != tt.destination) {
            // A walk ending after the latest time there is reaches nothing.
            if let Some(e) = *rode.get(fp.from)?
                && let Some(t) = e.checked_add_signed(fp.duration)
            {
                changed |= improve(walked.get_mut(fp.to)?, t);
            }
        }
        if !changed {
            let mut earliest: PerStation<_> =
                rode.into_iter().zip(walked).map(|(r, w)| r.into_iter().chain(w).min()).collect();
            *earliest.get_mut(start)? = Some(t0);
            return Ok(earliest);
        }
    }
}

/// Lower bound on the remaining travel time from each station, ignoring waits:
/// fastest ride per segment over the whole timetable, summed along the best path.
/// `ride` is a segment's time from its departure and arrival. Cancelled stops
/// are skipped (ICE 857 "Wuppertal 09:41 → Hagen 09:41"); a stop with only
/// one time is passed at that time.
fn lower_bounds(tt: &Timetable, ride: impl Fn(Time, Time) -> TimeDelta) -> Result<PerStation<Option<TimeDelta>>> {
    let mut reverse: PerStation<Vec<(StationIdx, TimeDelta)>> = tt.per_station(Vec::new());
    for trip in &tt.trips {
        let mut previous: Option<(StationIdx, Time)> = None;
        for call in trip.calls.iter().filter(|c| !c.cancelled) {
            if let (Some((from, departure)), Some(arrival)) = (previous, call.arrival.or(call.departure)) {
                let edge = (from, ride(departure, arrival).max(TimeDelta::zero()));
                reverse.get_mut(call.station)?.push(edge);
            }
            previous = call.departure.or(call.arrival).map(|d| (call.station, d));
        }
    }
    for fp in &tt.footpaths {
        reverse.get_mut(fp.to)?.push((fp.from, fp.duration));
    }
    // Saturating: a bound beyond the longest duration there is prunes all the same.
    shortest_paths(&reverse, tt.destination, TimeDelta::zero(), checked::sum)
}

/// Dijkstra from `source` over `edges` (station → (neighbour, weight)): the
/// least sum of weights to reach each station, `None` where unreachable.
/// `add` should saturate rather than overflow.
pub fn shortest_paths<W: Ord + Copy>(
    edges: &PerStation<Vec<(StationIdx, W)>>,
    source: StationIdx,
    zero: W,
    add: impl Fn(W, W) -> W,
) -> Result<PerStation<Option<W>>> {
    let mut best: PerStation<Option<W>> = edges.map(|_| None);
    let mut heap = BinaryHeap::new();
    *best.get_mut(source)? = Some(zero);
    heap.push(Reverse((zero, source)));
    while let Some(Reverse((d, s))) = heap.pop() {
        if best.get(s)?.is_some_and(|b| d > b) {
            continue;
        }
        for &(next, w) in edges.get(s)? {
            let nd = add(d, w);
            if improve(best.get_mut(next)?, nd) {
                heap.push(Reverse((nd, next)));
            }
        }
    }
    Ok(best)
}

/// Duration of the fastest real journey from each station to the destination,
/// waits and walks included, over all departures in the timetable (profile
/// connection scan). Any change time is allowed, as in the enumeration.
pub fn fastest_journeys(tt: &Timetable) -> Result<PerStation<Option<TimeDelta>>> {
    struct Ride {
        from: StationIdx,
        to: StationIdx,
        departure: DateTime<Utc>,
        arrival: DateTime<Utc>,
        trip: TripIdx,
        /// Positions of `from` and `to` in the trip.
        seq: (usize, usize),
        /// Whether the train can be boarded at `from`, not just passed.
        boardable: bool,
    }
    let mut rides = Vec::new();
    for (trip, t) in tt.trips.iter().enumerate() {
        let trip = TripIdx(trip);
        let mut previous: Option<(StationIdx, DateTime<Utc>, bool, usize)> = None;
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
    let mut walks: PerStation<Vec<(StationIdx, TimeDelta)>> = tt.per_station(Vec::new());
    for fp in &tt.footpaths {
        walks.get_mut(fp.from)?.push((fp.to, fp.duration));
    }

    // Per station, (departure, arrival at goal) with both falling: the earliest
    // arrival departing at or after t is the last entry departing at or after t.
    let mut profiles: PerStation<Vec<(DateTime<Utc>, DateTime<Utc>)>> = tt.per_station(Vec::new());
    let reach = |profiles: &PerStation<Vec<(DateTime<Utc>, DateTime<Utc>)>>, station: StationIdx, t: DateTime<Utc>| {
        let p = profiles.get(station)?;
        Ok::<_, anyhow::Error>(p.partition_point(|&(d, _)| d >= t).checked_sub(1).and_then(|k| p.get(k)).map(|e| e.1))
    };
    // Per trip and stop: earliest arrival at the goal when on the train leaving there.
    let mut staying: PerTrip<Vec<Option<DateTime<Utc>>>> = tt.trips.iter().map(|t| vec![None; t.calls.len()]).collect();
    let mut best: PerStation<Option<TimeDelta>> = tt.per_station(None);
    *best.get_mut(tt.destination)? = Some(TimeDelta::zero());
    // Whether the ride improved anything.
    let mut scan = |r: &Ride| {
        let no_call = |seq: usize| format!("trip {} has no call {seq}", r.trip.0);
        let stops = staying.get_mut(r.trip)?;
        let mut goal = *stops.get(r.seq.1).with_context(|| no_call(r.seq.1))?;
        let mut consider = |t: Option<DateTime<Utc>>| {
            if let Some(t) = t {
                goal = Some(goal.map_or(t, |g: DateTime<Utc>| g.min(t)));
            }
        };
        if r.to == tt.destination {
            consider(Some(r.arrival));
        }
        consider(reach(&profiles, r.to, r.arrival)?);
        for &(next, walk) in walks.get(r.to)? {
            // A walk ending after the latest time there is reaches nothing.
            let Some(walked) = r.arrival.checked_add_signed(walk) else { continue };
            if next == tt.destination {
                consider(Some(walked));
            }
            consider(reach(&profiles, next, walked)?);
        }
        let Some(goal) = goal else { return Ok(false) };
        let changed = improve(stops.get_mut(r.seq.0).with_context(|| no_call(r.seq.0))?, goal);
        if !r.boardable {
            return Ok(changed);
        }
        let p = profiles.get_mut(r.from)?;
        match p.last_mut() {
            Some(last) if last.1 <= goal => return Ok(changed),
            Some(last) if last.0 == r.departure => last.1 = goal,
            _ => p.push((r.departure, goal)),
        }
        let duration = goal.signed_duration_since(r.departure);
        improve(best.get_mut(r.from)?, duration);
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
        if let Some(after) = by_train.get(fp.to)?.map(|b| checked::sum(b, fp.duration)) {
            improve(best.get_mut(fp.from)?, after);
        }
    }
    Ok(best)
}

/// A train leaving a station: (trip, call, departure).
type Departure = (TripIdx, CallIdx, DateTime<Utc>);

/// Per-timetable indexes shared by all searches on it (main and fallbacks).
pub struct Network<'a> {
    pub tt: &'a Timetable,
    /// Per station: (trip, call, departure) sorted by departure.
    departures: PerStation<Vec<Departure>>,
    walks: PerStation<Vec<FootpathIdx>>,
    /// Best-case remaining travel time from each station, by planned ride
    /// times: live times of delayed trains are often squeezed to zero
    /// (ICE 1091 Ludwigshafen 05:15 → Mannheim 05:15 at +10/+6).
    pub lower_bound: PerStation<Option<TimeDelta>>,
    /// Like `lower_bound`, but never above a live ride time either (a delayed
    /// train catching up), so pruning by it keeps every connection within the limits.
    bound: PerStation<Option<TimeDelta>>,
}

impl<'a> Network<'a> {
    /// Fails if the timetable refers to a station it doesn't have.
    pub fn new(tt: &'a Timetable) -> Result<Self> {
        let mut departures: PerStation<Vec<Departure>> = tt.per_station(Vec::new());
        for (t, trip) in tt.trips.iter().enumerate() {
            for (c, call) in trip.calls.iter().enumerate().take(trip.calls.len().saturating_sub(1)) {
                if let Some(departure) = call.departure_time().filter(|_| !call.cancelled) {
                    departures.get_mut(call.station)?.push((TripIdx(t), CallIdx(c), departure));
                }
            }
        }
        for list in departures.iter_mut() {
            list.sort_by_key(|&(_, _, departure)| departure);
        }
        let mut walks: PerStation<Vec<FootpathIdx>> = tt.per_station(Vec::new());
        for (i, fp) in tt.footpaths.iter().enumerate() {
            walks.get_mut(fp.from)?.push(FootpathIdx(i));
        }
        tt.station(tt.origin)?;
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
    station: StationIdx,
    /// Arrival; `None` at the origin.
    arrival: Option<DateTime<Utc>>,
    /// From when trains there count as catchable (the origin: the chain's first departure).
    since: DateTime<Utc>,
    /// Buffer of the change made here, once the next train is chosen.
    slack_out: Option<TimeDelta>,
    /// Where a train boarded starts the connection (the origin, or where the
    /// walk from it leads): the walk from the origin before it.
    starts: Option<TimeDelta>,
}

/// What makes boarding a train from one earlier stop no detour after all:
/// the rest of the chain passes one of the stations, or arrives after the time.
type Unless = (Vec<StationIdx>, DateTime<Utc>);

struct Search<'a> {
    tt: &'a Timetable,
    limits: &'a Limits,
    departures: &'a PerStation<Vec<Departure>>,
    walks: &'a PerStation<Vec<FootpathIdx>>,
    lower_bound: &'a PerStation<Option<TimeDelta>>,
    bound: &'a PerStation<Option<TimeDelta>>,
    max_duration: TimeDelta,
    /// For each station visited, the chain's stop (index into `presence`)
    /// it was reached for: the origin 0, passed stations the stop they lead to.
    visited: PerStation<Option<usize>>,
    used: PerTrip<bool>,
    steps: Vec<Step>,
    /// The walk from the origin before the first train, if the chain starts with one.
    lead: TimeDelta,
    rides: usize,
    /// Rides allowed in the current deepening round.
    max_rides: usize,
    /// Stations where the chain stopped, and from when another train could be caught there.
    presence: Vec<Presence>,
    /// Per train boarded that is a detour unless the rest of the chain passes
    /// a station or arrives late: per earlier stop it could have been boarded
    /// at instead, the stations it would have passed from there that the chain
    /// had not been at, and the arrival up to which boarding it there would
    /// still have been listed. A connection passing none of one stop's
    /// stations and arriving by then is that detour.
    detour_unless: Vec<Vec<Unless>>,
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
    let starts = net.departures.get(limits.origin)?;
    // Trains from the origin, and from where a walk from it leads (leaving
    // the origin the walk's time earlier): (station, walk, departures).
    let mut starts: Vec<(StationIdx, TimeDelta, &[Departure])> = vec![(limits.origin, TimeDelta::zero(), starts)];
    for &footpath in net.walks.get(limits.origin)? {
        let fp = tt.footpath(footpath)?;
        if fp.to != tt.destination {
            starts.push((fp.to, fp.duration, net.departures.get(fp.to)?));
        }
    }
    let fastest_with = |missed_by| -> Result<Option<TimeDelta>> {
        let mut fastest: Option<TimeDelta> = None;
        for &(station, walk, departures) in &starts {
            // Saturating: a window beyond the latest time there is ends there.
            let (from, until) =
                (checked::later_or_max(limits.depart_from, walk), checked::later_or_max(limits.depart_until, walk));
            for &(_, _, d) in departures.iter().filter(|&&(_, _, d)| (from..=until).contains(&d)) {
                let arrivals = earliest_arrival(tt, station, d, until, missed_by)?;
                if let Some(a) = *arrivals.get(tt.destination)? {
                    let duration = checked::sum(a.signed_duration_since(d), walk);
                    fastest = Some(fastest.map_or(duration, |f| f.min(duration)));
                }
            }
        }
        Ok(fastest)
    };
    // Connections just missed are listed too, also when nothing else works.
    let (fastest, missed_by) = match fastest_with(TimeDelta::zero())? {
        None if limits.missed_by > TimeDelta::zero() => (fastest_with(limits.missed_by)?, limits.missed_by),
        fastest => (fastest, TimeDelta::zero()),
    };
    let Some(fastest_duration) = fastest else {
        return Ok(Enumeration {
            chains: Vec::new(),
            fastest: None,
            complete_up_to: Some(limits.max_changes),
            nodes: 0,
        });
    };
    let listing = list(net, limits, fastest_duration, report)?;
    // Earliest arrival can't see that no connection passes a station twice:
    // coming back through one (on a train not stopping there, or over changes
    // missed going back in time), it can get there sooner than any connection.
    // A connection listed as fast, missing no change by more than earliest
    // arrival allowed, shows it didn't.
    for chain in &listing.chains {
        if chain.duration(tt)? == fastest_duration
            && chain.min_slack(tt)?.is_none_or(|s| checked::sum(s, missed_by) >= TimeDelta::zero())
        {
            return Ok(listing);
        }
    }
    let exact = |missed_by| {
        fastest_with(missed_by)?
            .map_or_else(|| Ok(Fastest::Takes(None)), |lower| fastest_listed(net, limits, missed_by, lower))
    };
    let fastest = match exact(TimeDelta::zero())? {
        Fastest::Takes(None) if limits.missed_by > TimeDelta::zero() => exact(limits.missed_by)?,
        fastest => fastest,
    };
    match fastest {
        // Left out by the limits only.
        Fastest::Takes(Some(f)) if f == fastest_duration => Ok(listing),
        Fastest::Takes(Some(f)) => list(net, limits, f, report),
        Fastest::Takes(None) => Ok(Enumeration {
            chains: Vec::new(),
            fastest: None,
            complete_up_to: Some(limits.max_changes),
            nodes: listing.nodes,
        }),
        // The limit earliest arrival gives may leave out connections, with any number of changes.
        Fastest::CutShort => Ok(Enumeration { complete_up_to: None, ..listing }),
    }
}

/// The connections within `limits` taking at most `fastest` and the extra time.
fn list(net: &Network<'_>, limits: &Limits, fastest: TimeDelta, report: &dyn Fn(Progress)) -> Result<Enumeration> {
    // Saturating: a limit beyond the longest duration there is limits nothing.
    let mut search = Search::new(net, limits, checked::sum(fastest, limits.extra_time), report)?;
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
    Ok(Enumeration { chains, fastest: Some(fastest), complete_up_to, nodes: search.nodes })
}

/// How the search for the fastest connection came out.
enum Fastest {
    /// The fastest connection's duration; `None` if there is none.
    Takes(Option<TimeDelta>),
    /// The effort cap cut it short.
    CutShort,
}

/// The fastest connection the search can list with changes missed by up to
/// `missed_by`, taking `lower` (earliest arrival's) at the least: with any
/// number of changes, detours and rides away from the destination, as
/// earliest arrival, but never at a station twice. Searches up to a limit
/// from `lower` on, doubling what it exceeds `lower` by, until a connection
/// fits or the limit is the longest any connection can take.
fn fastest_listed(net: &Network<'_>, limits: &Limits, missed_by: TimeDelta, lower: TimeDelta) -> Result<Fastest> {
    let tt = net.tt;
    let relaxed = Limits { missed_by, keep_detours: true, forward_only: false, ..*limits };
    let quiet = |_: Progress| {};
    let mut search = Search::new(net, &relaxed, lower, &quiet)?;
    // Every ride ends at a station not visited yet.
    search.max_rides = tt.stations.len();
    // Leaving in the window, no connection arrives after the last train does and a walk.
    let last = tt.trips.iter().flat_map(|t| &t.calls).filter_map(Call::arrival_time).max();
    let walk = tt.footpaths.iter().map(|fp| fp.duration).max().unwrap_or_default();
    let longest = last.map_or(lower, |l| checked::sum(l.signed_duration_since(limits.depart_from), walk));
    loop {
        search.explore(limits.origin, limits.depart_from, None, false)?;
        if search.truncated {
            return Ok(Fastest::CutShort);
        }
        if !search.found.is_empty() {
            let mut fastest = None;
            for steps in search.found {
                improve(&mut fastest, Chain { steps }.duration(tt)?);
            }
            return Ok(Fastest::Takes(fastest));
        }
        if search.max_duration == longest {
            return Ok(Fastest::Takes(None));
        }
        // Saturating; a limit past `longest` finds nothing more.
        let excess = checked::difference(search.max_duration, lower).max(TimeDelta::minutes(1));
        search.max_duration = checked::sum(search.max_duration, excess).min(longest);
    }
}

impl<'a> Search<'a> {
    /// A search of `net` within `limits` for connections taking at most `max_duration`.
    fn new(
        net: &'a Network<'_>,
        limits: &'a Limits,
        max_duration: TimeDelta,
        report: &'a dyn Fn(Progress),
    ) -> Result<Self> {
        let tt = net.tt;
        let mut visited = tt.per_station(None);
        *visited.get_mut(limits.origin)? = Some(0);
        Ok(Self {
            tt,
            limits,
            departures: &net.departures,
            walks: &net.walks,
            lower_bound: &net.lower_bound,
            bound: &net.bound,
            max_duration,
            visited,
            used: tt.per_trip(false),
            steps: Vec::new(),
            lead: TimeDelta::zero(),
            rides: 0,
            max_rides: 0,
            presence: Vec::new(),
            detour_unless: Vec::new(),
            found: HashSet::new(),
            nodes: 0,
            truncated: false,
            report,
            report_at: next_report(),
        })
    }
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
        station: StationIdx,
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
            self.record(ready)?;
            return Ok(());
        }
        let (tt, departures, walks) = (self.tt, self.departures, self.walks);

        // From the origin, a walk to a nearby station first (Göteborg Central to
        // the bus terminal next door); then a train, as after any walk.
        if deadline.is_none() && !walked {
            for &footpath in walks.get(station)? {
                let fp = tt.footpath(footpath)?;
                let to = fp.to;
                // Saturating: a walk ending after the latest time there is reaches nothing in time.
                let ready = checked::later_or_max(self.limits.depart_from, fp.duration);
                if to == tt.destination || self.visited.get(to)?.is_some() {
                    continue;
                }
                // The origin, where a train calling there too is boarded instead,
                // and where the walk leads.
                self.presence.push(Presence {
                    station,
                    arrival: None,
                    since: self.limits.depart_from,
                    slack_out: None,
                    starts: Some(TimeDelta::zero()),
                });
                *self.visited.get_mut(to)? = Some(self.presence.len());
                self.presence.push(Presence {
                    station: to,
                    arrival: Some(ready),
                    since: ready,
                    slack_out: None,
                    starts: Some(fp.duration),
                });
                self.steps.push(Step::Walk { footpath });
                self.lead = fp.duration;
                self.explore(to, ready, None, true)?;
                self.lead = TimeDelta::zero();
                self.steps.pop();
                self.presence.truncate(self.presence.len().saturating_sub(2));
                *self.visited.get_mut(to)? = None;
                if self.truncated {
                    return Ok(());
                }
            }
        }
        if let Some(deadline) = deadline.filter(|_| !walked) {
            for &footpath in walks.get(station)? {
                let fp = tt.footpath(footpath)?;
                // A walk ending after the latest time there is misses any deadline.
                let Some(arrive) = ready.checked_add_signed(fp.duration) else { continue };
                let to = fp.to;
                if self.visited.get(to)?.is_some() || arrive > deadline || !self.reachable(to, arrive, deadline)? {
                    continue;
                }
                *self.visited.get_mut(to)? = Some(self.presence.len());
                self.presence.push(Presence {
                    station: to,
                    arrival: Some(arrive),
                    since: arrive,
                    slack_out: None,
                    starts: None,
                });
                self.steps.push(Step::Walk { footpath });
                self.explore(to, arrive, Some(deadline), true)?;
                self.steps.pop();
                self.presence.pop();
                *self.visited.get_mut(to)? = None;
            }
        }
        if self.rides >= self.max_rides {
            return Ok(());
        }

        // Trains may leave up to `missed_by` before the chain gets here; before
        // the earliest time there is, that's any train.
        let catchable_from = checked::earlier_or_min(ready, self.limits.missed_by);
        for &(trip, board, departure) in departures.get(station)? {
            if *self.used.get(trip)? {
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
                    // Leaving the origin a walk before the train, if there is one.
                    let leave = checked::earlier_or_min(departure, self.lead);
                    if leave < self.limits.depart_from {
                        continue;
                    }
                    if leave > self.limits.depart_until {
                        break;
                    }
                    // Saturating: a deadline after the latest time there is excludes nothing.
                    checked::later_or_max(leave, self.max_duration)
                }
            };
            let earlier_calls =
                calls.get(..board.0).with_context(|| format!("trip {} has no call {}", trip.0, board.0))?;
            let unless = if self.limits.keep_detours {
                Vec::new()
            } else {
                match self.detour(earlier_calls, departure, deadline)? {
                    Some(unless) => unless,
                    None => continue,
                }
            };
            let deferred = !unless.is_empty();
            if deferred {
                self.detour_unless.push(unless);
            }

            let first = self.rides == 0;
            match self.presence.last_mut() {
                // The change made here, for detour comparisons further on.
                Some(here) if !first => here.slack_out = here.arrival.map(|a| departure.signed_duration_since(a)),
                // Leaving later on the same train from the origin is never worse;
                // after a walk from there, the origin is left a walk before.
                _ => self.presence.push(Presence {
                    station,
                    arrival: None,
                    since: departure,
                    slack_out: None,
                    starts: Some(self.lead),
                }),
            }
            *self.used.get_mut(trip)? = true;
            // Below `max_rides` here, so this never saturates.
            self.rides = self.rides.saturating_add(1);
            let mut passed = Vec::new();
            // The stop each station passed leads to.
            let reached = self.presence.len();
            for (alight, call) in calls.iter().enumerate().skip(board.0.saturating_add(1)) {
                let alight = CallIdx(alight);
                let seen = self.visited.get_mut(call.station)?;
                if seen.is_some() {
                    break;
                }
                let Some(arrival) = call.arrival_time() else { break };
                // Arriving before departing means merged data disagrees; don't ride on it.
                if arrival > deadline || arrival < departure {
                    break;
                }
                *seen = Some(reached);
                passed.push(call.station);
                let worth_stopping = call.station == tt.destination
                    || !departures.get(call.station)?.is_empty()
                    || !walks.get(call.station)?.is_empty();
                let progress =
                    !self.limits.forward_only || self.lower_bound.get(call.station)? < self.lower_bound.get(station)?;
                if !call.cancelled && worth_stopping && progress && self.reachable(call.station, arrival, deadline)? {
                    let at = Presence {
                        station: call.station,
                        arrival: Some(arrival),
                        since: arrival,
                        slack_out: None,
                        starts: None,
                    };
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
                *self.visited.get_mut(s)? = None;
            }
            // Incremented above.
            self.rides = self.rides.saturating_sub(1);
            *self.used.get_mut(trip)? = false;
            if deferred {
                self.detour_unless.pop();
            }
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
    /// changes leave, and boarded there would have got here and on as the
    /// chain does: passing no station the chain had been at by then, nor one
    /// the rest of the chain passes. Detours that raise the worst buffer (S1
    /// to Altona instead of a short change at Hamburg Hbf) stay. At the
    /// origin, any later departure of the train counts; after a walk from
    /// there, also an earlier one, there or where the walk led, but only for
    /// a connection arriving within the travel time limit from leaving the
    /// origin for it. Either way only one leaving the origin within the
    /// window counts, as the search lists no other. `None` if it is a detour
    /// whatever follows (arriving by `deadline`); otherwise, per earlier stop
    /// it is a detour from unless the rest of the chain passes one or arrives
    /// later, the stations the train passes from there that the chain has not
    /// been at (none: no detour) and the latest arrival boarding there allows.
    fn detour(
        &self,
        earlier_calls: &[Call],
        departure: DateTime<Utc>,
        deadline: DateTime<Utc>,
    ) -> Result<Option<Vec<Unless>>> {
        let mut unless = Vec::new();
        let Some((here, before)) = self.presence.split_last() else { return Ok(Some(unless)) };
        // Worst buffer of the detour from an earlier stop up to boarding here.
        let mut worst = here.arrival.map_or(TimeDelta::MAX, |a| departure.signed_duration_since(a));
        for (stop, p) in before.iter().enumerate().rev() {
            let mut from_p = earlier_calls.iter().skip_while(|c| c.station != p.station);
            let caught =
                from_p.next().filter(|c| !c.cancelled).and_then(Call::departure_time).filter(|&d| d >= p.since);
            let comfortable = caught.is_some_and(|d| {
                p.arrival.is_none_or(|arrival| {
                    let direct = d.signed_duration_since(arrival);
                    direct >= self.limits.min_transfer && direct >= worst.min(p.slack_out.unwrap_or(TimeDelta::MAX))
                })
            });
            // Boarding where it starts the connection, it leaves the origin
            // a walk before the train: listed only within the window.
            let leaves = p.starts.zip(caught).map(|(walk, d)| checked::earlier_or_min(d, walk));
            if comfortable
                && leaves.is_none_or(|l| l <= self.limits.depart_until)
                && let Some(stations) = self.not_yet_passed(stop, from_p)?
            {
                // It may leave the origin earlier than the chain and take
                // longer: listed only if within the limit.
                let by = leaves.map_or(DateTime::<Utc>::MAX_UTC, |l| checked::later_or_max(l, self.max_duration));
                if stations.is_empty() && by >= deadline {
                    return Ok(None);
                }
                unless.push((stations, by));
            }
            worst = worst.min(p.slack_out.unwrap_or(TimeDelta::MAX));
        }
        Ok(Some(unless))
    }

    /// The stations of `calls` the chain has not been at, for a train boarded
    /// at the chain's stop `stop`; `None` if one is at a station the chain had
    /// been at by then, where its ride would end (cancelled stops included).
    fn not_yet_passed<'c>(
        &self,
        stop: usize,
        calls: impl Iterator<Item = &'c Call>,
    ) -> Result<Option<Vec<StationIdx>>> {
        let mut stations = Vec::new();
        for call in calls {
            match *self.visited.get(call.station)? {
                Some(reached) if reached <= stop => return Ok(None),
                Some(_) => {}
                None => stations.push(call.station),
            }
        }
        Ok(Some(stations))
    }

    /// Whether the destination can still be reached by `deadline` from
    /// `station` at `at`. Every train still to board may leave up to
    /// `missed_by` before the chain gets there.
    fn reachable(&self, station: StationIdx, at: DateTime<Utc>, deadline: DateTime<Utc>) -> Result<bool> {
        // `rides` never exceeds `max_rides`.
        let boardings = i32::try_from(self.max_rides.saturating_sub(self.rides)).unwrap_or(i32::MAX);
        let early = self.limits.missed_by.checked_mul(boardings).unwrap_or(TimeDelta::MAX);
        // Past the latest time there is: no deadline, or never there in time.
        Ok(self.bound.get(station)?.is_some_and(|lb| {
            deadline
                .checked_add_signed(early)
                .is_none_or(|d| at.checked_add_signed(lb).is_some_and(|earliest| earliest <= d))
        }))
    }

    /// Every variant counts, also ones riding the same trains but changing
    /// elsewhere (Ludwigshafen Mitte instead of Ludwigshafen Hbf), unless it
    /// is a detour after all: arriving at `arrival`, it passes none of the
    /// stations boarding a train earlier would have, and arrives in time for
    /// that connection to be listed.
    fn record(&mut self, arrival: DateTime<Utc>) -> Result<()> {
        for (stations, by) in self.detour_unless.iter().flatten() {
            let mut passed = arrival > *by;
            for &s in stations {
                passed |= self.visited.get(s)?.is_some();
            }
            if !passed {
                return Ok(());
            }
        }
        self.found.insert(self.steps.clone());
        Ok(())
    }
}

/// A worst buffer this long is enough: longer ones don't count as better, so
/// waiting 3 h at Lund for a later train doesn't beat taking the next one.
pub const ENOUGH_BUFFER: TimeDelta = TimeDelta::minutes(90);

/// The connections no other one beats: leaving no earlier, arriving no
/// later, changing no more often and with no smaller worst buffer up to
/// `ENOUGH_BUFFER` (a direct connection's counts as enough, a missed change's
/// as negative), better in one of them. Equal ones are all kept (the same
/// trains changing at Ludwigshafen Hbf or Mitte). Order: latest departure first.
pub fn best(tt: &Timetable, chains: Vec<Chain>) -> Result<Vec<Chain>> {
    type Key = (Reverse<DateTime<Utc>>, DateTime<Utc>, usize, Reverse<TimeDelta>);
    let mut keyed = chains
        .into_iter()
        .map(|c| {
            let slack = c.min_slack(tt)?.unwrap_or(ENOUGH_BUFFER).min(ENOUGH_BUFFER);
            Ok(((Reverse(c.departure(tt)?), c.arrival(tt)?, c.changes(), Reverse(slack)), c))
        })
        .collect::<Result<Vec<(Key, Chain)>>>()?;
    // A connection beating another sorts before it.
    keyed.sort_by_key(|k| k.0);
    let beats = |a: &Key, b: &Key| a != b && a.0 <= b.0 && a.1 <= b.1 && a.2 <= b.2 && a.3 <= b.3;
    let mut kept: Vec<(Key, Chain)> = Vec::new();
    for (key, chain) in keyed {
        if !kept.iter().any(|(k, _)| beats(k, &key)) {
            kept.push((key, chain));
        }
    }
    Ok(kept.into_iter().map(|(_, c)| c).collect())
}

/// Connections sharing first and last train.
pub struct Group {
    pub first: TripIdx,
    pub last: TripIdx,
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
    let mut groups: HashMap<(TripIdx, TripIdx), Vec<(Key, Chain)>> = HashMap::new();
    for chain in chains {
        let first = chain.rides().next().context("connection without a train")?.trip;
        let last = chain.rides().last().map_or(first, |ride| ride.trip);
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
