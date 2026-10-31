//! What to do when a change is missed: connections from the change station
//! after the missed departure, cut where they catch a train of the plan again.

use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;

use crate::checked;
use crate::enumerate::{self, Chain, Limits, Network, Step};
use crate::text;
use crate::timetable::Timetable;

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

/// Fallback connections per (station, earliest departure), computed once.
pub struct Fallbacks<'n> {
    net: &'n Network<'n>,
    min_transfer: TimeDelta,
    /// Where the fallbacks' times are shown in.
    zone: Tz,
    cache: HashMap<(usize, DateTime<Utc>), Vec<Chain>>,
}

impl<'n> Fallbacks<'n> {
    pub fn new(net: &'n Network<'n>, min_transfer: TimeDelta, zone: Tz) -> Self {
        Self { net, min_transfer, zone, cache: HashMap::new() }
    }

    /// What leaves `station` after the `missed` train, once there at `ready`.
    fn candidates(&mut self, station: usize, missed: DateTime<Utc>, ready: DateTime<Utc>) -> Result<&[Chain]> {
        let (net, min_transfer) = (self.net, self.min_transfer);
        let (after, until) = window(missed, ready)?;
        match self.cache.entry((station, after)) {
            Entry::Occupied(found) => Ok(found.into_mut()),
            Entry::Vacant(slot) => {
                let limits = Limits {
                    origin: station,
                    depart_from: after,
                    depart_until: until,
                    max_changes: CHANGES,
                    extra_time: EXTRA,
                    min_transfer,
                    // What to do once the change is missed: only what still works.
                    missed_by: TimeDelta::zero(),
                    keep_detours: false,
                    forward_only: false,
                    max_nodes: NODES,
                };
                Ok(slot.insert(enumerate::enumerate(net, &limits)?.chains))
            }
        }
    }

    /// The best fallbacks if the train after change `change` of `chain` is missed.
    /// Ranked by what happens until the plan is rejoined: from there on the trip
    /// continues as planned, so its arrival is the planned one.
    pub fn for_change(&mut self, chain: &Chain, change: usize) -> Result<Vec<Fallback>> {
        let tt = self.net.tt;
        let min_transfer = self.min_transfer;
        let transfers = chain.transfers(tt)?;
        let transfer = transfers.get(change).with_context(|| format!("the connection has no change {change}"))?;
        // The rides after the change; `change` is below the number of changes.
        let after_change = change.saturating_add(1);
        // Catching a later train of the plan (also the missed one, further on)
        // before the plan leaves it ends the detour. Trip → where the plan leaves it.
        let rejoin: HashMap<usize, usize> =
            chain.rides().skip(after_change).map(|(trip, _, alight)| (trip, alight)).collect();
        let arrival = chain.arrival(tt)?;

        // A change listed although missed (negative buffer): the traveller is
        // there only after the missed train left; nothing leaves before that.
        // From where the train arrived: after a change with a walk, fallbacks
        // walk there too, or elsewhere. And from where the missed train left:
        // a walk starting there counts only from after the missed departure,
        // but the traveller already stands there, and may leave right after it.
        let ready = transfer.ready();
        let mut candidates: Vec<Chain> = self.candidates(transfer.station, transfer.departure, ready)?.to_vec();
        if let Some((trip, board, _)) = chain.rides().nth(after_change) {
            let boards_at = tt.call(trip, board)?.station;
            if boards_at != transfer.station {
                // A fallback walking from one start to the other first is also
                // found from the other, with the earlier window: kept only there,
                // unless its train leaves after that window ends.
                let (_, until) = window(transfer.departure, ready)?;
                candidates = not_walking_to(tt, candidates, boards_at, until)?;
                let from_boarding = self.candidates(boards_at, transfer.departure, ready)?.to_vec();
                candidates.extend(not_walking_to(tt, from_boarding, transfer.station, until)?);
            }
        }
        let mut ranked = candidates
            .iter()
            .map(|fallback| {
                let rides: Vec<_> = fallback.rides().collect();
                let back =
                    rides.iter().position(|(trip, board, _)| rejoin.get(trip).is_some_and(|alight| board < alight));
                // Every fallback has a ride.
                let changes = back.unwrap_or_else(|| rides.len().saturating_sub(1));
                let slack = fallback.transfers(tt)?.iter().take(changes).map(|t| t.slack).min();
                let lands = if back.is_some() { arrival } else { fallback.arrival(tt)? };
                let marked: HashSet<usize> =
                    back.and_then(|i| rides.get(i)).map(|&(trip, _, _)| trip).into_iter().collect();
                let line = text::route(tt, fallback, min_transfer, &marked, self.zone)?;
                Ok((lands, changes, Reverse(slack), line, back, fallback))
            })
            .collect::<Result<Vec<_>>>()?;
        ranked.sort_by(|a, b| (a.0, a.1, a.2, &a.3).cmp(&(b.0, b.1, b.2, &b.3)));
        ranked.dedup_by(|a, b| a.3 == b.3);
        Ok(ranked
            .into_iter()
            .take(SHOWN)
            .map(|(lands, _, _, line, rejoins_at, chain)| Fallback { chain: chain.clone(), lands, rejoins_at, line })
            .collect())
    }
}

/// From when to when fallbacks may leave after the `missed` train, once there at `ready`.
fn window(missed: DateTime<Utc>, ready: DateTime<Utc>) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let after = checked::later(missed, TimeDelta::minutes(1), "departure after a missed train")?.max(ready);
    // Saturating: a window reaching past the latest time there is ends there.
    Ok((after, checked::later_or_max(after, WINDOW)))
}

/// `chains` without those walking to `to` first for a train leaving there by `until`.
fn not_walking_to(tt: &Timetable, chains: Vec<Chain>, to: usize, until: DateTime<Utc>) -> Result<Vec<Chain>> {
    let mut kept = Vec::with_capacity(chains.len());
    for c in chains {
        let walks_there = match c.steps.first() {
            Some(Step::Walk { footpath }) if tt.footpath(*footpath)?.to == to => match c.rides().next() {
                Some((trip, board, _)) => tt.call(trip, board)?.departure_time().is_some_and(|d| d <= until),
                None => false,
            },
            _ => false,
        };
        if !walks_there {
            kept.push(c);
        }
    }
    Ok(kept)
}

#[cfg(test)]
#[path = "fallback_tests.rs"]
mod tests;
