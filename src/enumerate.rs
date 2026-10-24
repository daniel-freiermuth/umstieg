//! Every connection within the limits, not just the Pareto-optimal ones.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use chrono::{DateTime, TimeDelta, Utc};

use crate::timetable::{Call, Timetable};

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
    /// Keep chains that take a train they could have boarded at an earlier stop.
    pub keep_detours: bool,
    /// Only rides that reduce the best-case remaining travel time. Off by
    /// default: riding away can pay off (S-Bahn to Heidelberg for an ECE
    /// that skips Mannheim).
    pub forward_only: bool,
    /// Search effort cap; deepening stops at the first change count exceeding it.
    pub max_nodes: usize,
}

#[derive(Debug, Clone, Copy)]
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

    pub fn departure(&self, tt: &Timetable) -> DateTime<Utc> {
        let (trip, board, _) = self.rides().next().expect("chain has a ride");
        tt.trips[trip].calls[board].departure_time().expect("boarded calls depart")
    }

    pub fn arrival(&self, tt: &Timetable) -> DateTime<Utc> {
        let (trip, _, alight) = self.rides().last().expect("chain has a ride");
        tt.trips[trip].calls[alight].arrival_time().expect("alighted calls arrive")
    }

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
                    previous = Some((calls[alight].station, calls[alight].arrival_time().expect("alighted calls arrive")));
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
    /// Shortest travel time found; the duration limit is based on it.
    pub fastest: Option<TimeDelta>,
    /// Highest number of changes for which the list is complete; below
    /// `max_changes` when the effort cap was hit.
    pub complete_up_to: Option<usize>,
    pub nodes: usize,
}

/// Earliest arrival at every station when starting at `start` at `t0`, with
/// changes of any length.
pub fn earliest_arrival(tt: &Timetable, start: usize, t0: DateTime<Utc>) -> Vec<Option<DateTime<Utc>>> {
    let mut ea = vec![None; tt.stations.len()];
    ea[start] = Some(t0);
    loop {
        let mut changed = false;
        for trip in &tt.trips {
            let mut aboard = false;
            for call in &trip.calls {
                if call.cancelled {
                    continue;
                }
                if aboard {
                    if let Some(a) = call.arrival_time().filter(|&a| ea[call.station].is_none_or(|e| a < e)) {
                        ea[call.station] = Some(a);
                        changed = true;
                    }
                } else {
                    aboard = matches!((ea[call.station], call.departure_time()), (Some(e), Some(d)) if e <= d);
                }
            }
        }
        for fp in &tt.footpaths {
            if let Some(e) = ea[fp.from] {
                let a = e + fp.duration;
                if ea[fp.to].is_none_or(|old| a < old) {
                    ea[fp.to] = Some(a);
                    changed = true;
                }
            }
        }
        if !changed {
            return ea;
        }
    }
}

/// Lower bound on the remaining travel time from each station, ignoring waits:
/// fastest ride per segment over the whole timetable, summed along the best path.
/// Ride times are planned ones: live times of delayed trains are often squeezed
/// to zero (ICE 1091 Ludwigshafen 05:15 → Mannheim 05:15 at +10/+6), and
/// cancelled stops are skipped (ICE 857 "Wuppertal 09:41 → Hagen 09:41").
fn lower_bounds(tt: &Timetable) -> Vec<Option<TimeDelta>> {
    let n = tt.stations.len();
    let mut reverse: Vec<Vec<(usize, TimeDelta)>> = vec![Vec::new(); n];
    for trip in &tt.trips {
        let mut previous: Option<(usize, DateTime<Utc>)> = None;
        for call in trip.calls.iter().filter(|c| !c.cancelled) {
            if let (Some((from, departure)), Some(arrival)) = (previous, call.arrival) {
                reverse[call.station].push((from, (arrival.planned - departure).max(TimeDelta::zero())));
            }
            previous = call.departure.map(|d| (call.station, d.planned));
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

/// Per-timetable indexes shared by all searches on it (main and fallbacks).
pub struct Network<'a> {
    pub tt: &'a Timetable,
    /// Per station: (trip, call) sorted by departure.
    departures: Vec<Vec<(usize, usize)>>,
    walks: Vec<Vec<usize>>,
    pub lower_bound: Vec<Option<TimeDelta>>,
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
        Self { tt, departures, walks, lower_bound: lower_bounds(tt) }
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
    max_duration: TimeDelta,
    visited: Vec<bool>,
    used: Vec<bool>,
    steps: Vec<Step>,
    rides: usize,
    /// Rides allowed in the current deepening round.
    max_rides: usize,
    /// Stations where the chain stopped, and from when another train could be caught there.
    presence: Vec<Presence>,
    /// Best variant per train sequence.
    found: HashMap<Vec<usize>, Chain>,
    nodes: usize,
    truncated: bool,
}

pub fn enumerate(net: &Network<'_>, limits: &Limits) -> Enumeration {
    let tt = net.tt;
    let fastest = net.departures[limits.origin]
        .iter()
        .filter_map(|&(t, c)| tt.trips[t].calls[c].departure_time())
        .filter(|d| (limits.depart_from..=limits.depart_until).contains(d))
        .filter_map(|d| Some(earliest_arrival(tt, limits.origin, d)[tt.destination]? - d))
        .min();
    let Some(fastest_duration) = fastest else {
        return Enumeration { chains: Vec::new(), fastest: None, complete_up_to: Some(limits.max_changes), nodes: 0 };
    };

    let mut search = Search {
        tt,
        limits,
        departures: &net.departures,
        walks: &net.walks,
        lower_bound: &net.lower_bound,
        max_duration: fastest_duration + limits.extra_time,
        visited: vec![false; tt.stations.len()],
        used: vec![false; tt.trips.len()],
        steps: Vec::new(),
        rides: 0,
        max_rides: 0,
        presence: Vec::new(),
        found: HashMap::new(),
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

    let chains = search.found.into_values().collect();
    Enumeration { chains, fastest, complete_up_to, nodes: search.nodes }
}

impl Search<'_> {
    /// At `station`, ready to leave at `ready`. `deadline` is set once the first train is chosen.
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
                if self.visited[to] || !self.reachable(to, arrive, deadline) {
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
                    if departure < ready {
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
                let progress =
                    !self.limits.forward_only || self.lower_bound[call.station] < self.lower_bound[station];
                if !call.cancelled && worth_stopping && progress && self.reachable(call.station, arrival, deadline) {
                    let at = Presence { station: call.station, arrival: Some(arrival), since: arrival, slack_out: None };
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
                        if direct >= self.limits.min_transfer && direct >= worst.min(p.slack_out.unwrap_or(TimeDelta::MAX)) {
                            return true;
                        }
                    }
                }
            }
            worst = worst.min(p.slack_out.unwrap_or(TimeDelta::MAX));
        }
        false
    }

    fn reachable(&self, station: usize, at: DateTime<Utc>, deadline: DateTime<Utc>) -> bool {
        self.lower_bound[station].is_some_and(|lb| at + lb <= deadline)
    }

    /// Keep one variant per train sequence: the one with the largest worst-case buffer.
    fn record(&mut self) {
        let chain = Chain { steps: self.steps.clone() };
        let key = chain.trips();
        let tt = self.tt;
        let score = |c: &Chain| (c.min_slack(tt).unwrap_or(TimeDelta::MAX), Reverse(c.arrival(tt)));
        match self.found.get(&key) {
            Some(existing) if score(existing) >= score(&chain) => {}
            _ => {
                self.found.insert(key, chain);
            }
        }
    }
}

/// Connections sharing first and last train.
pub struct Group {
    pub first: usize,
    pub last: usize,
    pub chains: Vec<Chain>,
}

/// Groups by (first train, last train), each sorted by arrival, changes, buffer;
/// groups ordered by their best connection.
pub fn group(tt: &Timetable, chains: Vec<Chain>) -> Vec<Group> {
    let mut groups: HashMap<(usize, usize), Vec<Chain>> = HashMap::new();
    for chain in chains {
        let trips = chain.trips();
        groups.entry((trips[0], trips[trips.len() - 1])).or_default().push(chain);
    }
    let key = |c: &Chain| (c.arrival(tt), c.changes(), Reverse(c.min_slack(tt).unwrap_or(TimeDelta::MAX)));
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
