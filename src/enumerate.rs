//! Every connection within the limits, not just the Pareto-optimal ones.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};

use crate::timetable::{Call, Time, Timetable};

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

    pub fn trips(&self) -> Vec<usize> {
        self.rides().map(|(trip, _, _)| trip).collect()
    }

    pub fn changes(&self) -> usize {
        self.rides().count() - 1
    }

    #[expect(clippy::expect_used, reason = "a chain has a ride, boarded at a call that departs")]
    pub fn departure(&self, tt: &Timetable) -> DateTime<Utc> {
        let (trip, board, _) = self.rides().next().expect("chain has a ride");
        tt.trips[trip].calls[board].departure_time().expect("boarded calls depart")
    }

    /// Arrival at the destination: the last train's, plus a walk after it.
    #[expect(clippy::expect_used, reason = "a chain has a ride, left at a call that arrives")]
    pub fn arrival(&self, tt: &Timetable) -> DateTime<Utc> {
        let walk: TimeDelta = self
            .steps
            .iter()
            .rev()
            .map_while(|s| match *s {
                Step::Walk { footpath } => Some(tt.footpaths[footpath].duration),
                Step::Ride { .. } => None,
            })
            .sum();
        let (trip, _, alight) = self.rides().last().expect("chain has a ride");
        tt.trips[trip].calls[alight].arrival_time().expect("alighted calls arrive") + walk
    }

    #[expect(clippy::expect_used, reason = "chains only board calls that depart and alight at calls that arrive")]
    pub fn transfers(&self, tt: &Timetable) -> Vec<Transfer> {
        let mut out = Vec::new();
        let mut previous: Option<(usize, DateTime<Utc>)> = None;
        let mut walk = TimeDelta::zero();
        for step in &self.steps {
            match *step {
                Step::Walk { footpath } => walk += tt.footpaths[footpath].duration,
                Step::Ride { trip, board, alight } => {
                    let calls = &tt.trips[trip].calls;
                    let departure = calls[board].departure_time().expect("boarded calls depart");
                    if let Some((station, arrival)) = previous {
                        out.push(Transfer { station, departure, walk, slack: departure - arrival - walk });
                    }
                    previous =
                        Some((calls[alight].station, calls[alight].arrival_time().expect("alighted calls arrive")));
                    walk = TimeDelta::zero();
                }
            }
        }
        out
    }

    pub fn min_slack(&self, tt: &Timetable) -> Option<TimeDelta> {
        self.transfers(tt).iter().map(|t| t.slack).min()
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
) -> Vec<Option<DateTime<Utc>>> {
    fn improve(slot: &mut Option<DateTime<Utc>>, t: DateTime<Utc>) -> bool {
        let better = slot.is_none_or(|e| t < e);
        if better {
            *slot = Some(t);
        }
        better
    }
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
                        changed |= improve(&mut rode[call.station], a);
                    }
                } else if let Some(d) = call.departure_time() {
                    aboard = if call.station == start {
                        (t0..=latest).contains(&d)
                    } else {
                        rode[call.station].into_iter().chain(walked[call.station]).any(|e| e - missed_by <= d)
                    };
                }
            }
        }
        for fp in tt.footpaths.iter().filter(|fp| fp.from != start) {
            if let Some(e) = rode[fp.from] {
                changed |= improve(&mut walked[fp.to], e + fp.duration);
            }
        }
        if !changed {
            return (0..n)
                .map(|s| if s == start { Some(t0) } else { rode[s].into_iter().chain(walked[s]).min() })
                .collect();
        }
    }
}

/// Lower bound on the remaining travel time from each station, ignoring waits:
/// fastest ride per segment over the whole timetable, summed along the best path.
/// `ride` is a segment's time from its departure and arrival. Cancelled stops
/// are skipped (ICE 857 "Wuppertal 09:41 → Hagen 09:41"); a stop with only
/// one time is passed at that time.
fn lower_bounds(tt: &Timetable, ride: impl Fn(Time, Time) -> TimeDelta) -> Vec<Option<TimeDelta>> {
    let n = tt.stations.len();
    let mut reverse: Vec<Vec<(usize, TimeDelta)>> = vec![Vec::new(); n];
    for trip in &tt.trips {
        let mut previous: Option<(usize, Time)> = None;
        for call in trip.calls.iter().filter(|c| !c.cancelled) {
            if let (Some((from, departure)), Some(arrival)) = (previous, call.arrival.or(call.departure)) {
                reverse[call.station].push((from, ride(departure, arrival).max(TimeDelta::zero())));
            }
            previous = call.departure.or(call.arrival).map(|d| (call.station, d));
        }
    }
    for fp in &tt.footpaths {
        reverse[fp.to].push((fp.from, fp.duration));
    }
    let mut best: Vec<Option<TimeDelta>> = vec![None; n];
    let mut heap = BinaryHeap::new();
    best[tt.destination] = Some(TimeDelta::zero());
    heap.push(Reverse((TimeDelta::zero(), tt.destination)));
    while let Some(Reverse((d, s))) = heap.pop() {
        if best[s].is_some_and(|b| d > b) {
            continue;
        }
        for &(prev, w) in &reverse[s] {
            let nd = d + w;
            if best[prev].is_none_or(|b| nd < b) {
                best[prev] = Some(nd);
                heap.push(Reverse((nd, prev)));
            }
        }
    }
    best
}

/// Duration of the fastest real journey from each station to the destination,
/// waits and walks included, over all departures in the timetable (profile
/// connection scan). Any change time is allowed, as in the enumeration.
pub fn fastest_journeys(tt: &Timetable) -> Vec<Option<TimeDelta>> {
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
        walks[fp.from].push((fp.to, fp.duration));
    }

    // Per station, (departure, arrival at goal) with both falling: the earliest
    // arrival departing at or after t is the last entry departing at or after t.
    let mut profiles: Vec<Vec<(DateTime<Utc>, DateTime<Utc>)>> = vec![Vec::new(); n];
    let reach = |profiles: &[Vec<(DateTime<Utc>, DateTime<Utc>)>], station: usize, t: DateTime<Utc>| {
        let p = &profiles[station];
        let k = p.partition_point(|&(d, _)| d >= t);
        (k > 0).then(|| p[k - 1].1)
    };
    // Per trip and stop: earliest arrival at the goal when on the train leaving there.
    let mut staying: Vec<Vec<Option<DateTime<Utc>>>> = tt.trips.iter().map(|t| vec![None; t.calls.len()]).collect();
    let mut best: Vec<Option<TimeDelta>> = vec![None; n];
    best[tt.destination] = Some(TimeDelta::zero());
    // Whether the ride improved anything.
    let mut scan = |r: &Ride| {
        let mut goal = staying[r.trip][r.seq.1];
        let mut consider = |t: Option<DateTime<Utc>>| {
            if let Some(t) = t {
                goal = Some(goal.map_or(t, |g: DateTime<Utc>| g.min(t)));
            }
        };
        if r.to == tt.destination {
            consider(Some(r.arrival));
        }
        consider(reach(&profiles, r.to, r.arrival));
        for &(next, walk) in &walks[r.to] {
            if next == tt.destination {
                consider(Some(r.arrival + walk));
            }
            consider(reach(&profiles, next, r.arrival + walk));
        }
        let Some(goal) = goal else { return false };
        let here = &mut staying[r.trip][r.seq.0];
        let changed = here.is_none_or(|g| goal < g);
        if changed {
            *here = Some(goal);
        }
        if !r.boardable {
            return changed;
        }
        let p = &mut profiles[r.from];
        match p.last_mut() {
            Some(last) if last.1 <= goal => return changed,
            Some(last) if last.0 == r.departure => last.1 = goal,
            _ => p.push((r.departure, goal)),
        }
        let duration = goal - r.departure;
        if best[r.from].is_none_or(|b| duration < b) {
            best[r.from] = Some(duration);
        }
        true
    };
    for same_time in rides.chunk_by(|a, b| a.departure == b.departure) {
        // Rides taking no time (squeezed live times) can lead onto each other
        // in any order: repeat until settled.
        let instant = same_time.iter().any(|r| r.arrival == r.departure);
        while same_time.iter().fold(false, |changed, r| scan(r) | changed) && instant {}
    }
    // Walking to a station and leaving from there by train (one walk, as
    // between trains: never two in a row).
    let by_train = best.clone();
    for fp in &tt.footpaths {
        if let Some(after) = by_train[fp.to].map(|b| b + fp.duration)
            && best[fp.from].is_none_or(|b| after < b)
        {
            best[fp.from] = Some(after);
        }
    }
    best
}

/// Per-timetable indexes shared by all searches on it (main and fallbacks).
pub struct Network<'a> {
    pub tt: &'a Timetable,
    /// Per station: (trip, call) sorted by departure.
    departures: Vec<Vec<(usize, usize)>>,
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
    pub fn new(tt: &'a Timetable) -> Self {
        let n = tt.stations.len();
        let mut departures: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n];
        for (t, trip) in tt.trips.iter().enumerate() {
            for (c, call) in trip.calls.iter().enumerate().take(trip.calls.len().saturating_sub(1)) {
                if call.departure_time().is_some() && !call.cancelled {
                    departures[call.station].push((t, c));
                }
            }
        }
        for list in &mut departures {
            list.sort_by_key(|&(t, c)| tt.trips[t].calls[c].departure_time());
        }
        let mut walks = vec![Vec::new(); n];
        for (i, fp) in tt.footpaths.iter().enumerate() {
            walks[fp.from].push(i);
        }
        Self {
            tt,
            departures,
            walks,
            lower_bound: lower_bounds(tt, |d, a| a.planned - d.planned),
            bound: lower_bounds(tt, |d, a| (a.planned - d.planned).min(a.actual - d.actual)),
        }
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
    departures: &'a [Vec<(usize, usize)>],
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
}

pub fn enumerate(net: &Network<'_>, limits: &Limits) -> Enumeration {
    let tt = net.tt;
    let fastest_with = |missed_by| {
        net.departures[limits.origin]
            .iter()
            .filter_map(|&(t, c)| tt.trips[t].calls[c].departure_time())
            .filter(|d| (limits.depart_from..=limits.depart_until).contains(d))
            .filter_map(|d| {
                Some(earliest_arrival(tt, limits.origin, d, limits.depart_until, missed_by)[tt.destination]? - d)
            })
            .min()
    };
    // Connections just missed are listed too, also when nothing else works.
    let fastest = fastest_with(TimeDelta::zero())
        .or_else(|| fastest_with(limits.missed_by).filter(|_| limits.missed_by > TimeDelta::zero()));
    let Some(fastest_duration) = fastest else {
        return Enumeration { chains: Vec::new(), fastest: None, complete_up_to: Some(limits.max_changes), nodes: 0 };
    };

    let mut search = Search {
        tt,
        limits,
        departures: &net.departures,
        walks: &net.walks,
        lower_bound: &net.lower_bound,
        bound: &net.bound,
        max_duration: fastest_duration + limits.extra_time,
        visited: vec![false; tt.stations.len()],
        used: vec![false; tt.trips.len()],
        steps: Vec::new(),
        rides: 0,
        max_rides: 0,
        presence: Vec::new(),
        found: HashSet::new(),
        nodes: 0,
        truncated: false,
    };
    search.visited[limits.origin] = true;
    // Deepen by changes so that hitting the effort cap still leaves complete
    // lists for few changes instead of an arbitrary depth-first subset.
    let mut complete_up_to = None;
    for changes in 0..=limits.max_changes {
        search.max_rides = changes + 1;
        search.explore(limits.origin, limits.depart_from, None, false);
        if search.truncated {
            break;
        }
        complete_up_to = Some(changes);
    }

    let chains = search.found.into_iter().map(|steps| Chain { steps }).collect();
    Enumeration { chains, fastest, complete_up_to, nodes: search.nodes }
}

impl Search<'_> {
    /// At `station`, ready to leave at `ready`. `deadline` is set once the first train is chosen.
    #[expect(
        clippy::too_many_lines,
        reason = "the search step: walks, then trains and their stops, sharing the backtracking state"
    )]
    fn explore(&mut self, station: usize, ready: DateTime<Utc>, deadline: Option<DateTime<Utc>>, walked: bool) {
        self.nodes += 1;
        if self.nodes > self.limits.max_nodes {
            self.truncated = true;
            return;
        }
        if station == self.tt.destination {
            self.record();
            return;
        }

        if let Some(deadline) = deadline.filter(|_| !walked) {
            for i in 0..self.walks[station].len() {
                let fp = &self.tt.footpaths[self.walks[station][i]];
                let (to, arrive) = (fp.to, ready + fp.duration);
                if self.visited[to] || arrive > deadline || !self.reachable(to, arrive, deadline) {
                    continue;
                }
                self.visited[to] = true;
                self.presence.push(Presence { station: to, arrival: Some(arrive), since: arrive, slack_out: None });
                self.steps.push(Step::Walk { footpath: self.walks[station][i] });
                self.explore(to, arrive, Some(deadline), true);
                self.steps.pop();
                self.presence.pop();
                self.visited[to] = false;
            }
        }
        if self.rides >= self.max_rides {
            return;
        }

        for i in 0..self.departures[station].len() {
            let (trip, board) = self.departures[station][i];
            if self.used[trip] {
                continue;
            }
            let calls = &self.tt.trips[trip].calls;
            let Some(departure) = calls[board].departure_time() else { continue };
            let deadline = match deadline {
                Some(d) if departure > d => break,
                Some(d) => {
                    if departure < ready - self.limits.missed_by {
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
                    departure + self.max_duration
                }
            };
            if !self.limits.keep_detours && self.is_detour(station, &calls[..board], departure) {
                continue;
            }

            let first = self.rides == 0;
            match self.presence.last_mut() {
                // The change made here, for detour comparisons further on.
                Some(here) if !first => here.slack_out = here.arrival.map(|a| departure - a),
                // Leaving later on the same train from the origin is never worse.
                _ => self.presence.push(Presence { station, arrival: None, since: departure, slack_out: None }),
            }
            self.used[trip] = true;
            self.rides += 1;
            let mut passed = Vec::new();
            for (alight, call) in calls.iter().enumerate().skip(board + 1) {
                if self.visited[call.station] {
                    break;
                }
                let Some(arrival) = call.arrival_time() else { break };
                // Arriving before departing means merged data disagrees; don't ride on it.
                if arrival > deadline || arrival < departure {
                    break;
                }
                self.visited[call.station] = true;
                passed.push(call.station);
                let worth_stopping = call.station == self.tt.destination
                    || !self.departures[call.station].is_empty()
                    || !self.walks[call.station].is_empty();
                let progress = !self.limits.forward_only || self.lower_bound[call.station] < self.lower_bound[station];
                if !call.cancelled && worth_stopping && progress && self.reachable(call.station, arrival, deadline) {
                    let at =
                        Presence { station: call.station, arrival: Some(arrival), since: arrival, slack_out: None };
                    self.presence.push(at);
                    self.steps.push(Step::Ride { trip, board, alight });
                    self.explore(call.station, arrival, Some(deadline), false);
                    self.steps.pop();
                    self.presence.pop();
                }
                if self.truncated {
                    break;
                }
            }
            for s in passed {
                self.visited[s] = false;
            }
            self.rides -= 1;
            self.used[trip] = false;
            if first {
                self.presence.pop();
            } else if let Some(here) = self.presence.last_mut() {
                here.slack_out = None;
            }
            if self.truncated {
                return;
            }
        }
    }

    /// Boarding this train here is a detour if it already called at a station
    /// the chain stopped at earlier, catchable there comfortably and with at
    /// least the buffer the detour's changes leave. Detours that raise the
    /// worst buffer (S1 to Altona instead of a short change at Hamburg Hbf)
    /// stay. At the origin, any later departure of the train counts.
    fn is_detour(&self, station: usize, earlier_calls: &[Call], departure: DateTime<Utc>) -> bool {
        let Some((here, before)) = self.presence.split_last() else { return false };
        debug_assert_eq!(here.station, station, "the chain stands where it boards");
        // Worst buffer of the detour from an earlier stop up to boarding here.
        let mut worst = here.arrival.map_or(TimeDelta::MAX, |a| departure - a);
        for p in before.iter().rev() {
            let caught = earlier_calls.iter().find(|c| c.station == p.station).and_then(Call::departure_time);
            if let Some(d) = caught.filter(|&d| d >= p.since) {
                match p.arrival {
                    None => return true,
                    Some(arrival) => {
                        let direct = d - arrival;
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
    fn reachable(&self, station: usize, at: DateTime<Utc>, deadline: DateTime<Utc>) -> bool {
        let boardings = i32::try_from(self.max_rides - self.rides).unwrap_or(i32::MAX);
        let early = self.limits.missed_by.checked_mul(boardings).unwrap_or(TimeDelta::MAX);
        self.bound[station].is_some_and(|lb| deadline.checked_add_signed(early).is_none_or(|d| at + lb <= d))
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
pub fn group(tt: &Timetable, chains: Vec<Chain>) -> Vec<Group> {
    let mut groups: HashMap<(usize, usize), Vec<Chain>> = HashMap::new();
    for chain in chains {
        let trips = chain.trips();
        groups.entry((trips[0], trips[trips.len() - 1])).or_default().push(chain);
    }
    let key = |c: &Chain| {
        let slack = c.min_slack(tt).unwrap_or(TimeDelta::MAX);
        (slack < TimeDelta::zero(), c.arrival(tt), c.changes(), Reverse(slack))
    };
    let mut out: Vec<Group> = groups
        .into_iter()
        .map(|((first, last), mut chains)| {
            chains.sort_by_key(key);
            Group { first, last, chains }
        })
        .collect();
    out.sort_by_key(|g| (key(&g.chains[0]), g.chains[0].departure(tt)));
    out
}

#[cfg(test)]
#[path = "enumerate_tests.rs"]
mod tests;
