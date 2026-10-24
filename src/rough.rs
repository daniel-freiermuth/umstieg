//! Rough routes from MOTIS. They decide which stations and trains the
//! timetable covers; the connections themselves are enumerated later.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use futures::future::join_all;

use crate::motis::{Itinerary, Leg, Motis, Stop};

/// How far past the latest hub arrival we look for onward connections.
const ONWARD_WINDOW: TimeDelta = TimeDelta::hours(3);

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
pub async fn collect(motis: &Motis, q: &Query) -> Result<Rough> {
    let baseline = motis
        .plan(&q.origin.id, &q.destination.id, q.time, q.window)
        .await?;
    let hubs = pick_hubs(&baseline, q);
    let halves = join_all(hubs.iter().map(|hub| via_hub(motis, q, hub))).await;

    let mut itineraries = baseline;
    let mut warnings = Vec::new();
    for (hub, result) in hubs.iter().zip(halves) {
        match result {
            Ok(found) => itineraries.extend(found),
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
            for place in [&pair[0].to, &pair[1].from] {
                if let Some(id) = place.station_id() {
                    *counts.entry((id, place.name.as_str())).or_default() += 1;
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
    let limit = hubs.len() + q.auto_hubs;
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

async fn via_hub(motis: &Motis, q: &Query, hub: &Stop) -> Result<Vec<Itinerary>> {
    let mut first = motis.plan(&q.origin.id, &hub.id, q.time, q.window).await?;
    let arrivals = first.iter().filter_map(|i| i.legs.last().map(|l| l.end_time));
    let (Some(lo), Some(hi)) = (arrivals.clone().min(), arrivals.max()) else {
        return Ok(first);
    };
    let second = motis
        .plan(&hub.id, &q.destination.id, lo, hi - lo + ONWARD_WINDOW)
        .await?;
    first.extend(second);
    Ok(first)
}
