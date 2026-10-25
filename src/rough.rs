//! Rough routes from MOTIS, plus an EFA journey planner for local legs where
//! stops are German. They decide which stations and trains the timetable
//! covers; the connections themselves are enumerated later.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use futures::future::join_all;

use crate::checked;
use crate::efa::{self, Efa};
use crate::motis::{Itinerary, Leg, Motis, Stop};

/// How far past the latest hub arrival we look for onward connections.
const ONWARD_WINDOW: TimeDelta = TimeDelta::hours(3);
/// How far before MOTIS's earliest hub arrival onward connections are collected.
const EARLIER_ONWARD: TimeDelta = TimeDelta::hours(2);
/// Width of the departure slices the planner is asked for separately.
const SLICE: TimeDelta = TimeDelta::hours(1);
/// EFA answers with several trips per request; ask it this often across a window.
const EFA_STEP: TimeDelta = TimeDelta::hours(1);

pub struct Query {
    pub origin: Stop,
    pub destination: Stop,
    pub time: DateTime<Utc>,
    pub window: TimeDelta,
    /// Hubs forced by the user, tried in addition to automatic ones.
    pub vias: Vec<Stop>,
    pub auto_hubs: usize,
}

pub struct Rough {
    pub itineraries: Vec<Itinerary>,
    pub hubs: Vec<String>,
    pub warnings: Vec<String>,
}

/// The planner's answers plus origin→hub and hub→destination halves for hubs
/// taken from those answers. Halves reach trains the planner never combines.
/// With an EFA planner, the halves are also asked there.
pub async fn collect(motis: &Motis, efa: Option<&Efa>, q: &Query) -> Result<Rough> {
    let answers = sliced(motis, &q.origin.id, &q.destination.id, q.time, q.window).await?;
    // Later departures the planner pads with still show the corridor, which
    // matters when only local services (missing in Transitous) run in the window.
    let hubs = pick_hubs(&answers, q);
    let baseline = departing(answers, q.time, q.window);
    let halves = join_all(hubs.iter().map(|hub| via_hub(motis, efa, q, hub))).await;

    let mut itineraries = baseline;
    let mut warnings = Vec::new();
    if let (Some(efa), Some(from), Some(to)) = (efa, efa::dhid(&q.origin.id), efa::dhid(&q.destination.id)) {
        itineraries.extend(efa_window(efa, &from, &to, q.time, q.window, &mut warnings).await);
    }
    for (hub, result) in hubs.iter().zip(halves) {
        match result {
            Ok((found, notes)) => {
                itineraries.extend(found);
                warnings.extend(notes);
            }
            Err(e) => warnings.push(format!("hub {}: {e:#}", hub.name)),
        }
    }
    Ok(Rough { itineraries, hubs: hubs.into_iter().map(|h| h.name).collect(), warnings })
}

/// Stations where the planner's answers change trains, most used first, plus the user's vias.
fn pick_hubs(baseline: &[Itinerary], q: &Query) -> Vec<Stop> {
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    for itinerary in baseline {
        let transit: Vec<&Leg> = itinerary.legs.iter().filter(|l| l.is_transit()).collect();
        for pair in transit.windows(2) {
            let [arriving, leaving] = pair else { continue };
            for place in [&arriving.to, &leaving.from] {
                if let Some(id) = place.station_id() {
                    let count = counts.entry((id, place.name.as_str())).or_default();
                    *count = count.saturating_add(1);
                }
            }
        }
    }
    let mut ranked: Vec<_> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Feeds of different countries name the same station alike but with different ids.
    let mut names: HashSet<String> = [q.origin.name.clone(), q.destination.name.clone()].into();
    let mut hubs = Vec::new();
    for stop in &q.vias {
        if names.insert(stop.name.clone()) {
            hubs.push(stop.clone());
        }
    }
    let limit = hubs.len().saturating_add(q.auto_hubs);
    for ((id, name), _) in ranked {
        if hubs.len() >= limit {
            break;
        }
        if names.insert(name.to_owned()) {
            hubs.push(Stop { id: id.to_owned(), name: name.to_owned(), position: None });
        }
    }
    hubs
}

async fn via_hub(motis: &Motis, efa: Option<&Efa>, q: &Query, hub: &Stop) -> Result<(Vec<Itinerary>, Vec<String>)> {
    let mut notes = Vec::new();
    // Sliced like the other requests: one wide window would lose earlier
    // departures that a later one matches on arrival.
    let mut first = departing(sliced(motis, &q.origin.id, &hub.id, q.time, q.window).await?, q.time, q.window);
    let (origin, at_hub, destination) = (efa::dhid(&q.origin.id), efa::dhid(&hub.id), efa::dhid(&q.destination.id));
    if let (Some(efa), Some(o), Some(h)) = (efa, &origin, &at_hub) {
        first.extend(efa_window(efa, o, h, q.time, q.window, &mut notes).await);
    }
    let arrivals = first.iter().filter_map(|i| i.legs.last().map(|l| l.end_time));
    let (Some(lo), Some(hi)) = (arrivals.clone().min(), arrivals.max()) else {
        return Ok((first, notes));
    };
    // MOTIS's own arrivals at the hub can be later than what the DB-corrected
    // timetable reaches (a delay making its best change infeasible, a stop it
    // doesn't know), so onward trains are collected from before them as well.
    let from = checked::earlier_or_min(lo, EARLIER_ONWARD).max(q.time);
    // A window: saturating only widens it past anything a feed answers.
    let onward = checked::sum(hi.signed_duration_since(from), ONWARD_WINDOW);
    let second = departing(sliced(motis, &hub.id, &q.destination.id, from, onward).await?, from, onward);
    first.extend(second);
    if let (Some(efa), Some(h), Some(d)) = (efa, &at_hub, &destination) {
        first.extend(efa_window(efa, h, d, from, onward, &mut notes).await);
    }
    Ok((first, notes))
}

/// EFA journeys departing within the window. Failures only cost coverage.
async fn efa_window(
    efa: &Efa,
    from: &str,
    to: &str,
    time: DateTime<Utc>,
    window: TimeDelta,
    notes: &mut Vec<String>,
) -> Vec<Itinerary> {
    let starts = starts(time, window, EFA_STEP);
    let mut found = Vec::new();
    for result in join_all(starts.iter().map(|&t| efa.trips(from, to, t))).await {
        match result {
            Ok(trips) => found.extend(trips),
            Err(e) => notes.push(format!("EFA {from} → {to}: {e:#}")),
        }
    }
    departing(found, time, window)
}

/// The planner drops a departure if a later one arrives as early (06:38 ICE
/// 378 at Mannheim vs. 08:03 ICE 774, both reaching Göteborg at 22:05). Such
/// earlier departures are the buffer-rich alternatives this tool is about, so
/// the window is asked for in slices: each slice keeps its own best departures.
/// Answers are unfiltered: MOTIS pads them with later departures.
async fn sliced(motis: &Motis, from: &str, to: &str, time: DateTime<Utc>, window: TimeDelta) -> Result<Vec<Itinerary>> {
    let starts = starts(time, window, SLICE);
    let slices = futures::future::try_join_all(starts.iter().map(|&t| motis.plan(from, to, t, SLICE))).await?;
    Ok(slices.into_iter().flatten().collect())
}

/// Request times `step` apart across the window. Its end saturates: a
/// window reaching past representable time just ends there.
fn starts(time: DateTime<Utc>, window: TimeDelta, step: TimeDelta) -> Vec<DateTime<Utc>> {
    let end = checked::later_or_max(time, window);
    let mut starts = Vec::new();
    let mut t = Some(time);
    while let Some(at) = t.filter(|&at| at < end) {
        starts.push(at);
        t = at.checked_add_signed(step);
    }
    starts
}

/// MOTIS pads short answers with later departures, up to days later when
/// nothing runs; keep the asked-for window.
fn departing(itineraries: Vec<Itinerary>, time: DateTime<Utc>, window: TimeDelta) -> Vec<Itinerary> {
    let span = time..=checked::later_or_max(time, window);
    itineraries
        .into_iter()
        .filter(|i| i.legs.iter().find(|l| l.is_transit()).is_some_and(|l| span.contains(&l.start_time)))
        .collect()
}

#[cfg(test)]
#[path = "rough_tests.rs"]
mod tests;
