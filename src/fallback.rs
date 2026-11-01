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
use crate::usable::Usable;

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
        let after = checked::later(missed, TimeDelta::minutes(1), "departure after a missed train")?.max(ready);
        match self.cache.entry((station, after)) {
            Entry::Occupied(found) => Ok(found.into_mut()),
            Entry::Vacant(slot) => {
                let limits = Limits {
                    origin: station,
                    depart_from: after,
                    // Saturating: a window reaching past the latest time there is ends there.
                    depart_until: checked::later_or_max(after, WINDOW),
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

    /// Every fallback of every change a connection on `network` can make (a
    /// train's arrival, maybe a walk, then any later train there, or one
    /// missed by up to `missed_by`): with their stretches the page finds the
    /// fallbacks of connections it finds itself.
    pub fn for_network(&mut self, network: &Usable, missed_by: TimeDelta) -> Result<Vec<Chain>> {
        let tt = self.net.tt;
        // Per station where trains leave: (where the train arrived, when one is ready there).
        let mut ready: HashMap<usize, Vec<(usize, DateTime<Utc>)>> = HashMap::new();
        for run in &network.rides {
            for k in run.from.saturating_add(1)..=run.to {
                let call = tt.call(run.trip, k)?;
                let Some(arrival) = call.arrival_time() else { continue };
                ready.entry(call.station).or_default().push((call.station, arrival));
                for walk in &network.walks {
                    let fp = tt.footpath(walk.footpath)?;
                    if fp.from == call.station {
                        let there = checked::later(arrival, fp.duration, "walk after a train")?;
                        ready.entry(fp.to).or_default().push((call.station, there));
                    }
                }
            }
        }
        let mut changes = HashSet::new();
        for run in &network.rides {
            for k in run.from..run.to {
                let call = tt.call(run.trip, k)?;
                let Some(departure) = call.departure_time() else { continue };
                for &(station, at) in ready.get(&call.station).into_iter().flatten() {
                    if checked::earlier_or_min(at, missed_by) <= departure {
                        changes.insert((station, departure, at));
                    }
                }
            }
        }
        let mut all = Vec::new();
        for (station, departure, at) in changes {
            all.extend_from_slice(self.candidates(station, departure, at)?);
        }
        Ok(all)
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
                // From there, only what boards a train right there, and doesn't
                // come back for what is listed from where the train arrived:
                // the fallbacks are shown from there, so a walk first would be
                // shown as one from there, and the rest is a second copy.
                let listed: HashSet<&[Step]> = candidates.iter().map(|c| c.steps.as_slice()).collect();
                let mut from_boarding = Vec::new();
                for c in self.candidates(boards_at, transfer.departure, ready)? {
                    if !walks_first(c) && !rides_back(tt, c, transfer.station, &listed)? {
                        from_boarding.push(c.clone());
                    }
                }
                // The same chain from its first train on, boarded without a walk,
                // is one fallback: boarded where it calls first. Walked to, it is
                // one of its own, unless also boarded at that call without a walk.
                // Earliest boarding first: of the copies in a slot, the first stays.
                // Copies tying on boarding and walk are the same chain.
                let mut copies: Vec<(Onward, Boarding, Chain)> = candidates
                    .into_iter()
                    .chain(from_boarding)
                    // Every fallback has a ride.
                    .filter_map(|c| onward(&c).map(|(key, rank)| (key, rank, c)))
                    .collect();
                copies.sort_by_key(|&(_, rank, _)| rank);
                let mut kept: HashMap<(Onward, Option<usize>), Chain> = HashMap::new();
                let mut direct: HashMap<Onward, HashSet<usize>> = HashMap::new();
                for (key, (board, walk), c) in copies {
                    if walk.is_none() {
                        direct.entry(key.clone()).or_default().insert(board);
                    }
                    kept.entry((key, walk.map(|_| board))).or_insert(c);
                }
                kept.retain(|(key, walked_to), _| {
                    walked_to.is_none_or(|board| !direct.get(key).is_some_and(|boards| boards.contains(&board)))
                });
                candidates = kept.into_values().collect();
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
                // A first train from where the missed one left is named with its station.
                let line = text::route_from(tt, fallback, Some(transfer.station), min_transfer, &marked, self.zone)?;
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

fn walks_first(chain: &Chain) -> bool {
    matches!(chain.steps.first(), Some(Step::Walk { .. }))
}

/// The first train of a chain, where it is left, and all steps after it.
type Onward = (usize, usize, Vec<Step>);
/// Where the first train is boarded, and the walk there if any.
type Boarding = (usize, Option<usize>);

/// `chain` from its first train on, without where that train is boarded; and that boarding.
fn onward(chain: &Chain) -> Option<(Onward, Boarding)> {
    let first = chain.steps.iter().position(|s| matches!(s, Step::Ride { .. }))?;
    let Step::Ride { trip, board, alight } = *chain.steps.get(first)? else { return None };
    let walk = match chain.steps.first() {
        Some(&Step::Walk { footpath }) => Some(footpath),
        _ => None,
    };
    Some(((trip, alight, chain.steps.get(first.saturating_add(1)..)?.to_vec()), (board, walk)))
}

/// Whether `chain` gets to `station` (by train or on foot, or riding through
/// it after a change) and goes on from there as one of the `listed` chains does.
/// Riding through on the first train is that chain boarded elsewhere (`onward`).
fn rides_back(tt: &Timetable, chain: &Chain, station: usize, listed: &HashSet<&[Step]>) -> Result<bool> {
    let mut changed = false;
    for (i, step) in chain.steps.iter().enumerate() {
        let rest = chain.steps.get(i.saturating_add(1)..).unwrap_or_default();
        let reached = match *step {
            Step::Ride { trip, board, alight } => {
                if changed {
                    for k in board.saturating_add(1)..alight {
                        if tt.call(trip, k)?.station == station {
                            let mut on = vec![Step::Ride { trip, board: k, alight }];
                            on.extend_from_slice(rest);
                            if listed.contains(on.as_slice()) {
                                return Ok(true);
                            }
                        }
                    }
                }
                changed = true;
                tt.call(trip, alight)?.station
            }
            Step::Walk { footpath } => tt.footpath(footpath)?.to,
        };
        if reached == station && listed.contains(rest) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
#[path = "fallback_tests.rs"]
mod tests;
