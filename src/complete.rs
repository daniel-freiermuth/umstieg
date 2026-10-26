//! Stops of trains known only from DB boards, from Transitous, after the search.
//!
//! A board gives a train's times only at the station it belongs to, so a train
//! no Transitous answer rode (FLX 1236: Frankfurt Süd → Hamburg via Berlin) has
//! calls only where boards were fetched and looks like a slow nonstop train.
//! For the trains of the listed connections, Transitous' copy fills in the
//! stops in between: for the display; the search has run already.

use anyhow::{Context, Result};
use chrono::TimeDelta;

use crate::enumerate::{Chain, Step};
use crate::motis::{Leg, Motis};
use crate::timetable::Timetable;

/// Trains looked up per search: two requests each, Transitous is volunteer-run.
pub const MAX_TRAINS: usize = 20;
/// Known calls this far apart hide stops worth asking for.
const GAP: TimeDelta = TimeDelta::minutes(20);
/// Departures asked for at a station: enough for a busy hub's few minutes.
const DEPARTURES: usize = 30;
/// Same train in both feeds: departures this close at the same station.
const SAME_TIME: TimeDelta = TimeDelta::minutes(2);

/// Trains ridden in `chains` known only from DB boards, with known calls at
/// least `GAP` apart: the widest gaps first, at most `MAX_TRAINS`.
pub fn wanted(tt: &Timetable, chains: &[Chain]) -> Vec<usize> {
    let mut trips: Vec<usize> = chains
        .iter()
        .flat_map(|c| &c.steps)
        .filter_map(|s| match *s {
            Step::Ride { trip, .. } => Some(trip),
            Step::Walk { .. } => None,
        })
        .collect();
    trips.sort_unstable();
    trips.dedup();
    let mut gapped: Vec<(TimeDelta, usize)> = trips
        .into_iter()
        .filter_map(|i| {
            let trip = tt.trips.get(i)?;
            if trip.transitous {
                return None;
            }
            let calls = &trip.calls;
            let times: Vec<_> = calls.iter().filter_map(|c| c.arrival.or(c.departure).map(|t| t.planned)).collect();
            let gap = times.windows(2).filter_map(|w| Some(w.get(1)?.signed_duration_since(*w.first()?))).max()?;
            (gap >= GAP).then_some((gap, i))
        })
        .collect();
    gapped.sort_unstable_by(|a, b| b.cmp(a));
    gapped.into_iter().take(MAX_TRAINS).map(|(_, i)| i).collect()
}

/// Transitous' copy of `trip`, found among the departures at its first known
/// call with a departure at a station Transitous has an id for: same time,
/// and the train number in its name or trip id (`FlixTrain` has it only there).
/// `None` if Transitous doesn't know the train there.
pub async fn lookup(motis: &Motis, tt: &Timetable, trip: usize) -> Result<Option<Leg>> {
    let t = tt.trip(trip)?;
    let anchor = t.calls.iter().find_map(|c| {
        let departure = c.departure?.planned;
        let id = tt.stations.get(c.station)?.motis_id.clone()?;
        Some((id, departure))
    });
    let Some((stop, departure)) = anchor else {
        tracing::debug!(train = t.label, "no departure at a station Transitous has an id for");
        return Ok(None);
    };
    let from = departure.checked_sub_signed(SAME_TIME).context("departure out of range")?;
    let found = motis.departures(&stop, from, DEPARTURES).await?.into_iter().find(|st| {
        let at = st.place.scheduled_departure.or(st.place.departure);
        let names = [st.trip_short_name.as_deref(), st.display_name.as_deref(), Some(st.trip_id.as_str())];
        at.is_some_and(|at| at.signed_duration_since(departure).abs() <= SAME_TIME)
            && t.numbers.iter().any(|&n| names.into_iter().flatten().any(|name| mentions(name, n)))
    });
    let Some(st) = found else {
        tracing::debug!(train = t.label, stop, %departure, "not among Transitous' departures there");
        return Ok(None);
    };
    Ok(Some(motis.trip(&st.trip_id).await?))
}

/// Whether `text` has `number` as one of its runs of digits ("ICE 614",
/// "…_FLX10-1236-0515…", "011174").
fn mentions(text: &str, number: u32) -> bool {
    text.split(|c: char| !c.is_ascii_digit()).any(|digits| digits.parse::<u32>().ok() == Some(number))
}

/// Points `chains`' rides of `trip` at its calls' new places after stops
/// were added (`remap[old] = new`).
pub fn remap(chains: &mut [Chain], trip: usize, remap: &[usize]) {
    for step in chains.iter_mut().flat_map(|c| &mut c.steps) {
        if let Step::Ride { trip: t, board, alight } = step
            && *t == trip
        {
            *board = remap.get(*board).copied().unwrap_or(*board);
            *alight = remap.get(*alight).copied().unwrap_or(*alight);
        }
    }
}

#[cfg(test)]
#[path = "complete_tests.rs"]
mod tests;
