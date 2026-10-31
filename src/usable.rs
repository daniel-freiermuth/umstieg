//! What the connections within the limits use, together: every ride, wait
//! and walk once, however many connections share it. The plot draws this
//! network; only the best connections come with their routes, so the page
//! stays small however many connections there are (396 000 from Frankfurt
//! Süd to Hamburg in an hour).

use std::collections::{BTreeSet, HashSet};

use chrono::{DateTime, Utc};

use crate::enumerate::{Chain, Step};
use crate::timetable::Timetable;

/// Times as unix seconds.
type Secs = i64;

/// A train run ridden through on some connection: calls `from..=to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RideRun {
    pub trip: usize,
    pub from: usize,
    pub to: usize,
}

/// Waiting at a station on some connection, merged per station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    pub station: usize,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// A walk taken on some connection, starting then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Walk {
    pub footpath: usize,
    pub start: DateTime<Utc>,
}

/// Everything on some connection.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Usable {
    pub rides: Vec<RideRun>,
    pub waits: Vec<Wait>,
    pub walks: Vec<Walk>,
}

fn time(s: Secs) -> DateTime<Utc> {
    DateTime::from_timestamp(s, 0).unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// What `chains` use: their rides as runs of a train (split where none of
/// them rides through a stop), their waits merged per station (a change missed
/// by a little from the departure to the arrival), their walks.
pub fn of_chains<'c>(tt: &Timetable, chains: impl IntoIterator<Item = &'c Chain>) -> Usable {
    // (trip, call k): stretch k → k + 1 ridden; standing at call k.
    let mut stretches: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut through: HashSet<(usize, usize)> = HashSet::new();
    let mut waits: Vec<(usize, Secs, Secs)> = Vec::new();
    let mut walks: Vec<(usize, Secs)> = Vec::new();
    for chain in chains {
        let Ok(start) = chain.departure(tt) else { continue };
        let mut here: Option<(usize, Secs)> = None;
        for step in &chain.steps {
            match *step {
                Step::Walk { footpath } => {
                    let Some(fp) = tt.footpaths.get(footpath) else { continue };
                    let at = here.map_or_else(|| start.timestamp(), |(_, t)| t);
                    walks.push((footpath, at));
                    here = here.map(|(_, t)| (fp.to, t.saturating_add(fp.duration.num_seconds())));
                }
                Step::Ride { trip, board, alight } => {
                    let time = |k: usize, departs: bool| {
                        let call = tt.call(trip, k).ok()?;
                        if departs { call.departure_time() } else { call.arrival_time() }.map(|t| t.timestamp())
                    };
                    if let (Some((s, ready)), Some(dep)) = (here, time(board, true)) {
                        waits.push((s, ready.min(dep), ready.max(dep)));
                    }
                    stretches.extend((board..alight).map(|k| (trip, k)));
                    through.extend((board.saturating_add(1)..alight).map(|k| (trip, k)));
                    here = tt.call(trip, alight).ok().zip(time(alight, false)).map(|(c, t)| (c.station, t));
                }
            }
        }
    }
    let mut out = Usable::default();
    let mut run: Option<RideRun> = None;
    for (trip, k) in stretches {
        let to = k.saturating_add(1);
        run = match run {
            Some(r) if r.trip == trip && r.to == k && through.contains(&(trip, k)) => Some(RideRun { to, ..r }),
            r => {
                out.rides.extend(r);
                Some(RideRun { trip, from: k, to })
            }
        };
    }
    out.rides.extend(run);
    waits.sort_unstable();
    for (station, a, b) in waits {
        match out.waits.last_mut() {
            Some(w) if w.station == station && a <= w.to.timestamp() => w.to = w.to.max(time(b)),
            _ => out.waits.push(Wait { station, from: time(a), to: time(b) }),
        }
    }
    walks.sort_unstable();
    walks.dedup();
    out.walks = walks.into_iter().map(|(footpath, start)| Walk { footpath, start: time(start) }).collect();
    out
}

#[cfg(test)]
#[path = "usable_tests.rs"]
mod tests;
