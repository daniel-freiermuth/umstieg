//! Journey search that keeps connections planners discard.
//!
//! Planners return the Pareto set over (departure, arrival, changes) and drop
//! everything else, including trips that wait longer at a change for the same
//! final train. We split the trip at hub stations, combine the halves ourselves
//! and keep the transfer buffer as an extra Pareto criterion.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use futures::future::join_all;

use crate::db::{Check, Db, Kind};
use crate::motis::{Itinerary, Leg, Motis, Stop};

/// How far past the latest hub arrival we look for onward connections.
const ONWARD_WINDOW: TimeDelta = TimeDelta::hours(3);
/// Search window for "what if I miss this change" queries.
const FALLBACK_WINDOW: TimeDelta = TimeDelta::hours(4);

pub struct Query {
    pub origin: Stop,
    pub destination: Stop,
    pub time: DateTime<Utc>,
    pub window: TimeDelta,
    /// Hubs forced by the user, tried in addition to automatic ones.
    pub vias: Vec<Stop>,
    pub auto_hubs: usize,
    /// Minimum buffer at a change we assemble ourselves.
    pub min_transfer: TimeDelta,
    /// Buffers above this count as equally comfortable when comparing trips.
    pub enough_buffer: TimeDelta,
}

#[derive(Debug, Clone)]
pub struct Journey {
    pub legs: Vec<Leg>,
    /// Hub the journey was assembled at; `None` if the planner returned it directly.
    pub via_hub: Option<String>,
    /// Whether the plain origin→destination query returned it.
    pub planner_shows: bool,
}

pub struct Transfer<'a> {
    pub arriving: &'a Leg,
    pub departing: &'a Leg,
    /// Waiting time left after walking.
    pub slack: TimeDelta,
}

impl Journey {
    fn new(legs: Vec<Leg>, via_hub: Option<String>) -> Option<Self> {
        legs.iter().any(Leg::is_transit).then_some(Self { legs, via_hub, planner_shows: false })
    }

    pub fn departure(&self) -> DateTime<Utc> {
        self.legs[0].start_time
    }

    pub fn arrival(&self) -> DateTime<Utc> {
        self.legs[self.legs.len() - 1].end_time
    }

    pub fn changes(&self) -> usize {
        self.legs.iter().filter(|l| l.is_transit()).count() - 1
    }

    pub fn transfers(&self) -> Vec<Transfer<'_>> {
        let mut out = Vec::new();
        let mut arriving: Option<&Leg> = None;
        let mut walk = TimeDelta::zero();
        for leg in &self.legs {
            if !leg.is_transit() {
                walk += leg.duration();
                continue;
            }
            if let Some(prev) = arriving {
                out.push(Transfer {
                    arriving: prev,
                    departing: leg,
                    slack: leg.start_time - prev.end_time - walk,
                });
            }
            arriving = Some(leg);
            walk = TimeDelta::zero();
        }
        out
    }

    pub fn min_slack(&self) -> Option<TimeDelta> {
        self.transfers().iter().map(|t| t.slack).min()
    }

    fn cancelled(&self) -> bool {
        self.legs.iter().any(|l| l.cancelled)
    }

    /// Trains taken and where they are boarded; identifies a journey.
    fn signature(&self) -> Vec<(String, String)> {
        self.legs
            .iter()
            .filter_map(|l| {
                let trip = l.trip_id.clone()?;
                Some((trip, l.from.station_id().unwrap_or(&l.from.name).to_owned()))
            })
            .collect()
    }

    fn dominates(&self, other: &Self, enough_buffer: TimeDelta) -> bool {
        let buffer = |j: &Self| j.min_slack().unwrap_or(TimeDelta::MAX).min(enough_buffer);
        let (d, a, c, b) = (self.departure(), self.arrival(), self.changes(), buffer(self));
        let (od, oa, oc, ob) = (other.departure(), other.arrival(), other.changes(), buffer(other));
        d >= od && a <= oa && c <= oc && b >= ob && (d > od || a < oa || c < oc || b > ob)
    }
}

/// What happens if a change is missed.
pub enum Recovery {
    /// Earliest arrival at the destination from the change station, and its first train.
    Reroute { arrival: DateTime<Utc>, first_leg: Box<Leg> },
    /// Nothing reaches the destination within the search window.
    Stranded,
    Failed(String),
}

pub struct Evaluated {
    pub journey: Journey,
    /// One entry per transfer; empty if fallbacks were not requested.
    pub recoveries: Vec<Recovery>,
}

/// A journey the planner offered that DB's live data contradicts.
pub struct Rejected {
    pub journey: Journey,
    pub reason: String,
}

pub struct Outcome {
    pub journeys: Vec<Evaluated>,
    pub rejected: Vec<Rejected>,
    pub hubs: Vec<String>,
    pub candidates: usize,
    pub warnings: Vec<String>,
}

pub async fn search(motis: &Motis, db: Option<&Db>, q: &Query, with_fallbacks: bool) -> Result<Outcome> {
    let baseline: Vec<Journey> = motis
        .plan(&q.origin.id, &q.destination.id, q.time, q.window)
        .await?
        .into_iter()
        .filter_map(|i| Journey::new(i.legs, None))
        .collect();
    let shown: HashSet<_> = baseline.iter().map(Journey::signature).collect();

    let hubs = pick_hubs(&baseline, q);
    let results = join_all(hubs.iter().map(|hub| via_hub(motis, q, hub))).await;

    let mut warnings = Vec::new();
    let mut all = baseline;
    for (hub, result) in hubs.iter().zip(results) {
        match result {
            Ok(journeys) => all.extend(journeys),
            Err(e) => warnings.push(format!("hub {}: {e:#}", hub.name)),
        }
    }
    let candidates = all.len();

    // MOTIS pads short answers with later departures; keep the asked-for window.
    let window = q.time..=q.time + q.window;
    let mut seen = HashSet::new();
    all.retain(|j| window.contains(&j.departure()) && !j.cancelled() && seen.insert(j.signature()));
    for j in &mut all {
        j.planner_shows = shown.contains(&j.signature());
    }

    let (mut front, rejected) = match db {
        Some(db) => verified_front(db, all, q.enough_buffer, &mut warnings).await,
        None => {
            let pool: Vec<_> = all.into_iter().map(Some).collect();
            let front = pareto_front(&pool, q.enough_buffer);
            (front.into_iter().filter_map(|i| pool[i].clone()).collect(), Vec::new())
        }
    };
    front.sort_by_key(|j| (j.departure(), j.arrival()));

    let journeys = if with_fallbacks {
        join_all(front.into_iter().map(|j| evaluate(motis, q, j))).await
    } else {
        front
            .into_iter()
            .map(|journey| Evaluated { journey, recoveries: Vec::new() })
            .collect()
    };

    Ok(Outcome { journeys, rejected, hubs: hubs.into_iter().map(|h| h.name).collect(), candidates, warnings })
}

/// Indices of journeys no other journey dominates.
fn pareto_front(pool: &[Option<Journey>], enough_buffer: TimeDelta) -> Vec<usize> {
    let live: Vec<(usize, &Journey)> = pool.iter().enumerate().filter_map(|(i, j)| Some((i, j.as_ref()?))).collect();
    live.iter()
        .filter(|(_, j)| !live.iter().any(|(_, o)| o.dominates(j, enough_buffer)))
        .map(|&(i, _)| i)
        .collect()
}

/// Pareto front where every member survived the DB check. Rejecting a member
/// can surface journeys it dominated, so verify until the front is stable.
async fn verified_front(
    db: &Db,
    all: Vec<Journey>,
    enough_buffer: TimeDelta,
    warnings: &mut Vec<String>,
) -> (Vec<Journey>, Vec<Rejected>) {
    let mut pool: Vec<Option<Journey>> = all.into_iter().map(Some).collect();
    let mut verified = HashSet::new();
    let mut rejected = Vec::new();
    loop {
        let front = pareto_front(&pool, enough_buffer);
        let todo: Vec<usize> = front.iter().copied().filter(|i| !verified.contains(i)).collect();
        if todo.is_empty() {
            return (front.into_iter().filter_map(|i| pool[i].take()).collect(), rejected);
        }
        let checks = todo.iter().filter_map(|&i| pool[i].clone()).map(|j| verify(db, j));
        let results = join_all(checks).await;
        for (i, (result, leg_warnings)) in todo.into_iter().zip(results) {
            verified.insert(i);
            warnings.extend(leg_warnings);
            match result {
                Ok(journey) => pool[i] = Some(journey),
                Err(reason) => {
                    if let Some(journey) = pool[i].take() {
                        rejected.push(Rejected { journey, reason });
                    }
                }
            }
        }
    }
}

/// Confirm each train at its boarding and alighting station against DB and take
/// over DB's times and platforms. `Err` explains why the journey is not possible.
async fn verify(db: &Db, mut journey: Journey) -> (Result<Journey, String>, Vec<String>) {
    let mut warnings = Vec::new();
    for leg in journey.legs.iter_mut().filter(|l| l.is_transit()) {
        // DB boards list trains only; buses, trams and metros stay unchecked.
        let class = match leg.mode.as_str() {
            "HIGHSPEED_RAIL" | "LONG_DISTANCE" | "NIGHT_RAIL" => 'F',
            "REGIONAL_FAST_RAIL" | "REGIONAL_RAIL" => 'N',
            "SUBURBAN" => 'S',
            _ => continue,
        };
        let Some(number) = leg.train_number() else { continue };
        let (departure, arrival) = tokio::join!(
            db.check(&leg.from, number, class, leg.scheduled_start_time, Kind::Departure),
            db.check(&leg.to, number, class, leg.scheduled_end_time, Kind::Arrival),
        );
        for (kind, check) in [(Kind::Departure, departure), (Kind::Arrival, arrival)] {
            match check {
                Err(e) => warnings.push(format!("DB check of {} failed: {e:#}", leg.label())),
                Ok(Check::NoData) => {}
                Ok(Check::Cancelled { station }) => {
                    return (Err(format!("{} is cancelled at {station}", leg.label())), warnings);
                }
                Ok(Check::NotServed { station }) => {
                    let verb = match kind {
                        Kind::Departure => "does not depart from",
                        Kind::Arrival => "does not stop at",
                    };
                    return (Err(format!("{} {verb} {station} according to DB", leg.label())), warnings);
                }
                Ok(Check::Confirmed { planned, actual, live, platform }) => {
                    // Without a DB change, a live Transitous time is still the better estimate.
                    let keep_transitous = !live && leg.real_time;
                    let (scheduled, time, place, confirmed) = match kind {
                        Kind::Departure => (
                            &mut leg.scheduled_start_time,
                            &mut leg.start_time,
                            &mut leg.from,
                            &mut leg.confirmed.departure,
                        ),
                        Kind::Arrival => (
                            &mut leg.scheduled_end_time,
                            &mut leg.end_time,
                            &mut leg.to,
                            &mut leg.confirmed.arrival,
                        ),
                    };
                    if !keep_transitous {
                        *scheduled = planned;
                        *time = actual;
                    }
                    if platform.is_some() {
                        place.track = platform;
                    }
                    *confirmed = true;
                    leg.real_time |= live;
                }
            }
        }
    }
    if let Some(t) = journey.transfers().iter().find(|t| t.slack < TimeDelta::zero()) {
        let reason = format!(
            "change at {} is broken: {} short",
            t.arriving.to.name,
            -t.slack.num_minutes()
        );
        return (Err(reason), warnings);
    }
    (Ok(journey), warnings)
}

/// Transfer stations of the planner's own answers, most used first, plus the user's vias.
fn pick_hubs(baseline: &[Journey], q: &Query) -> Vec<Stop> {
    let endpoints: HashSet<&str> = baseline
        .iter()
        .flat_map(|j| {
            let first = &j.legs[0].from;
            let last = &j.legs[j.legs.len() - 1].to;
            [first.station_id(), last.station_id()]
        })
        .flatten()
        .collect();

    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    for j in baseline {
        for t in j.transfers() {
            for place in [&t.arriving.to, &t.departing.from] {
                if let Some(id) = place.station_id().filter(|id| !endpoints.contains(id)) {
                    *counts.entry((id, place.name.as_str())).or_default() += 1;
                }
            }
        }
    }
    let mut ranked: Vec<_> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut names = HashSet::new();
    let mut hubs = Vec::new();
    for stop in &q.vias {
        if names.insert(stop.name.clone()) {
            hubs.push(stop.clone());
        }
    }
    // Feeds of different countries name the same station alike but with different ids.
    for ((id, name), _) in ranked {
        if hubs.len() >= q.vias.len() + q.auto_hubs {
            break;
        }
        if names.insert(name.to_owned()) {
            hubs.push(Stop { id: id.to_owned(), name: name.to_owned() });
        }
    }
    hubs
}

async fn via_hub(motis: &Motis, q: &Query, hub: &Stop) -> Result<Vec<Journey>> {
    let first = motis.plan(&q.origin.id, &hub.id, q.time, q.window).await?;
    let Some((lo, hi)) = first
        .iter()
        .filter_map(|i| i.legs.last().map(|l| l.end_time))
        .fold(None, |acc: Option<(DateTime<Utc>, DateTime<Utc>)>, t| {
            Some(acc.map_or((t, t), |(lo, hi)| (lo.min(t), hi.max(t))))
        })
    else {
        return Ok(Vec::new());
    };
    let second = motis
        .plan(&hub.id, &q.destination.id, lo, hi - lo + ONWARD_WINDOW)
        .await?;

    Ok(first
        .iter()
        .flat_map(|a| second.iter().map(move |b| (a, b)))
        .filter_map(|(a, b)| combine(a, b, q.min_transfer))
        .filter_map(|legs| Journey::new(legs, Some(hub.name.clone())))
        .collect())
}

/// Join an itinerary ending at a hub with one starting there.
fn combine(a: &Itinerary, b: &Itinerary, min_transfer: TimeDelta) -> Option<Vec<Leg>> {
    let a_end = a.legs.iter().rposition(Leg::is_transit)?;
    let b_start = b.legs.iter().position(Leg::is_transit)?;
    let arriving = &a.legs[a_end];
    let departing = &b.legs[b_start];

    let mut legs = a.legs[..a_end].to_vec();
    if arriving.trip_id == departing.trip_id {
        // Same train through the hub: stay seated.
        if departing.start_time < arriving.end_time {
            return None;
        }
        let mut through = arriving.clone();
        through.to = departing.to.clone();
        through.end_time = departing.end_time;
        through.scheduled_end_time = departing.scheduled_end_time;
        legs.push(through);
    } else {
        let walks = a.legs[a_end + 1..].iter().chain(&b.legs[..b_start]);
        let walk: TimeDelta = walks.clone().map(Leg::duration).sum();
        if departing.start_time - arriving.end_time - walk < min_transfer {
            return None;
        }
        legs.push(arriving.clone());
        legs.extend(walks.cloned());
        legs.push(departing.clone());
    }
    legs.extend_from_slice(&b.legs[b_start + 1..]);
    Some(legs)
}

async fn evaluate(motis: &Motis, q: &Query, journey: Journey) -> Evaluated {
    let queries: Vec<_> = journey
        .transfers()
        .iter()
        .map(|t| {
            let station = t.arriving.to.station_id().map(str::to_owned);
            let name = t.arriving.to.name.clone();
            let after = t.departing.start_time + TimeDelta::minutes(1);
            async move {
                let Some(station) = station else {
                    return Recovery::Failed(format!("{name} has no stop id"));
                };
                recovery(motis, q, &station, after)
                    .await
                    .unwrap_or_else(|e| Recovery::Failed(format!("{e:#}")))
            }
        })
        .collect();
    let recoveries = join_all(queries).await;
    Evaluated { journey, recoveries }
}

async fn recovery(motis: &Motis, q: &Query, station: &str, after: DateTime<Utc>) -> Result<Recovery> {
    let best = motis
        .plan(station, &q.destination.id, after, FALLBACK_WINDOW)
        .await?
        .into_iter()
        .filter_map(|i| Journey::new(i.legs, None))
        .min_by_key(Journey::arrival);
    Ok(match best {
        Some(best) => {
            let arrival = best.arrival();
            let first_leg = best.legs.into_iter().find(Leg::is_transit).expect("Journey has a transit leg");
            Recovery::Reroute { arrival, first_leg: Box::new(first_leg) }
        }
        None => Recovery::Stranded,
    })
}
