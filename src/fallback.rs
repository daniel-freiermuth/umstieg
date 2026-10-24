//! What to do when a change is missed: connections from the change station
//! after the missed departure, cut where they catch a train of the plan again.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};

use crate::enumerate::{self, Chain, Limits, Network};
use crate::text;

/// How long after a missed train fallbacks may leave, and how they're bounded.
pub const WINDOW: TimeDelta = TimeDelta::hours(2);
const EXTRA: TimeDelta = TimeDelta::hours(1);
const CHANGES: usize = 3;
const NODES: usize = 500_000;
const SHOWN: usize = 3;

pub struct Fallback {
    pub chain: Chain,
    /// Arrival at the destination: the planned one if the plan is rejoined.
    pub lands: DateTime<Utc>,
    /// Index of the ride that is back on a train of the plan.
    pub rejoins_at: Option<usize>,
    /// One-line description, cut at the rejoin.
    pub line: String,
}

/// Fallback connections per (station, missed departure), computed once.
pub struct Fallbacks<'n> {
    net: &'n Network<'n>,
    min_transfer: TimeDelta,
    cache: HashMap<(usize, DateTime<Utc>), Vec<Chain>>,
}

impl<'n> Fallbacks<'n> {
    pub fn new(net: &'n Network<'n>, min_transfer: TimeDelta) -> Self {
        Self { net, min_transfer, cache: HashMap::new() }
    }

    fn candidates(&mut self, station: usize, missed: DateTime<Utc>) -> &[Chain] {
        let (net, min_transfer) = (self.net, self.min_transfer);
        self.cache.entry((station, missed)).or_insert_with(|| {
            let after = missed + TimeDelta::minutes(1);
            let limits = Limits {
                origin: station,
                depart_from: after,
                depart_until: after + WINDOW,
                max_changes: CHANGES,
                extra_time: EXTRA,
                min_transfer,
                keep_detours: false,
                forward_only: false,
                max_nodes: NODES,
            };
            enumerate::enumerate(net, &limits).chains
        })
    }

    /// The best fallbacks if the train after change `change` of `chain` is missed.
    /// Ranked by what happens until the plan is rejoined: from there on the trip
    /// continues as planned, so its arrival is the planned one.
    pub fn for_change(&mut self, chain: &Chain, change: usize) -> Vec<Fallback> {
        let tt = self.net.tt;
        let min_transfer = self.min_transfer;
        let transfer = &chain.transfers(tt)[change];
        let trips = chain.trips();
        // Catching a later train of the plan (also the missed one, further on) ends the detour.
        let rejoin: HashSet<usize> = trips[change + 1..].iter().copied().collect();
        let arrival = chain.arrival(tt);

        let mut ranked: Vec<_> = self
            .candidates(transfer.station, transfer.departure)
            .iter()
            .map(|fallback| {
                let rides = fallback.trips();
                let back = rides.iter().position(|t| rejoin.contains(t));
                let changes = back.unwrap_or(rides.len() - 1);
                let slack = fallback.transfers(tt)[..changes].iter().map(|t| t.slack).min();
                let lands = if back.is_some() { arrival } else { fallback.arrival(tt) };
                let line = text::route(tt, fallback, min_transfer, &rejoin);
                (lands, changes, Reverse(slack), line, back, fallback)
            })
            .collect();
        ranked.sort_by(|a, b| (a.0, a.1, a.2, &a.3).cmp(&(b.0, b.1, b.2, &b.3)));
        ranked.dedup_by(|a, b| a.3 == b.3);
        ranked
            .into_iter()
            .take(SHOWN)
            .map(|(lands, _, _, line, rejoins_at, chain)| Fallback { chain: chain.clone(), lands, rejoins_at, line })
            .collect()
    }
}
