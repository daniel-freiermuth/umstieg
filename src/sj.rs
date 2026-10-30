//! SJ's booking API, the one sj.se uses: its connection search over every
//! Swedish operator, trains and long-distance buses (Vy's express buses), with
//! combinations no other planner offers (SJ Regional 319 to Emmaboda for the
//! Krösatåg to Karlskrona). Not an open API: an extra source of rough routes,
//! asked when a search passes through Sweden. When it fails, the search goes
//! on without it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeDelta, Utc};
use chrono_tz::Europe::Stockholm;
use serde::Deserialize;

use crate::cache::DiskCache;
use crate::motis::{Itinerary, Leg, Place};
use crate::rough::Query;
use crate::trafikverket;

/// The API (`--sj` overrides it; empty: not asked).
pub const API: &str = "https://prod-api.adp.sj.se/public/sales/booking/v3";
/// The key sj.se itself sends with every request.
const KEY: &str = "d6625619def348d38be070027fd24ff6";
/// A search answers a whole day; repeating or tweaking it reuses that.
const CACHE_TTL: Duration = Duration::from_mins(10);
/// Time zone of SJ's dates.
const SWEDISH_TIME_ZONE: &str = "Europe/Stockholm";
/// Days asked for at most: SJ answers a day per search.
const MAX_DAYS: usize = 3;
/// A change between stops nearby is walked in at most this (Malmö C Norra
/// Vallgatan to Malmö Central), however long SJ's change is.
const MAX_WALK: TimeDelta = TimeDelta::minutes(10);
/// From the station asked about to a nearby stop a connection starts at.
const END_WALK: TimeDelta = TimeDelta::minutes(5);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Search {
    departure_search_id: String,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    travels: Vec<Travel>,
}

#[derive(Deserialize)]
struct Travel {
    #[serde(default)]
    departures: Vec<Departure>,
}

/// One connection as SJ offers it: between the stations asked about, its
/// legs maybe from or to another stop nearby (a bus terminal).
#[derive(Debug, Clone, Deserialize)]
pub struct Departure {
    origin: SjStation,
    destination: SjStation,
    #[serde(default)]
    legs: Vec<SjLeg>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SjLeg {
    origin: SjStation,
    destination: SjStation,
    departure_date_time: DateTime<FixedOffset>,
    arrival_date_time: DateTime<FixedOffset>,
    #[serde(default)]
    service_name: String,
    service_identifier: Option<String>,
    service_schedule_date: Option<String>,
    #[serde(default)]
    transport_method: String,
    service_type: Option<ServiceType>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SjStation {
    uic_station_code: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceType {
    modality: Option<String>,
    description: Option<String>,
    operator_name: Option<String>,
}

pub struct Sj {
    http: reqwest::Client,
    base: String,
    cache: DiskCache,
    requests: AtomicUsize,
}

impl Sj {
    pub fn new(base: &str, user_agent: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().user_agent(user_agent).timeout(Duration::from_secs(30)).build()?,
            base: base.trim_end_matches('/').to_owned(),
            cache: DiskCache::new("sj", CACHE_TTL),
            requests: AtomicUsize::new(0),
        })
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<String> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let resp = request
            .header("ocp-apim-subscription-key", KEY)
            .header("x-client-name", "sjse-booking-client")
            .header("accept-language", "sv-SE")
            .send()
            .await
            .context("SJ")?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("SJ: {status} {}", text.chars().take(200).collect::<String>());
        }
        Ok(text)
    }

    /// SJ's connections from `from` to `to` (UIC codes) on `date`, all day.
    pub async fn departures(&self, from: &str, to: &str, date: NaiveDate) -> Result<Vec<Departure>> {
        let fetch = async {
            let body = serde_json::json!({
                "origin": from, "destination": to, "departureDate": date.to_string(),
                "passengers": [{ "passengerCategory": { "type": "ADULT" } }],
            });
            let search = self.send(self.http.post(format!("{}/search", self.base)).json(&body)).await?;
            let search: Search = serde_json::from_str(&search).context("SJ's search")?;
            let url = format!("{}/departures/search/{}", self.base, search.departure_search_id);
            self.send(self.http.get(url)).await
        };
        let text = self.cache.get_or_fetch(&format!("{from} {to} {date}"), fetch).await?;
        let answer: Answer = serde_json::from_str(&text).context("SJ's departures")?;
        Ok(answer.travels.into_iter().flat_map(|t| t.departures).collect())
    }
}

/// The UIC code of a Swedish stop from its Transitous id: Trafiklab's
/// "se-Trafiklab_740000073" is it; Rejseplanen's "dk-rejseplanen_000007400073"
/// has the station's UIC number 74 00073 in it.
pub fn uic(stop_id: &str) -> Option<String> {
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if let Some(code) = stop_id.strip_prefix("se-Trafiklab_").filter(|c| c.len() == 9 && c.starts_with("740")) {
        return digits(code).then(|| code.to_owned());
    }
    let code = stop_id.strip_prefix("dk-rejseplanen_")?.trim_start_matches('0');
    let station = code.strip_prefix("74").filter(|s| s.len() == 5 && digits(s))?;
    Some(format!("7400{station}"))
}

/// SJ's connections leaving in `[from, until]`, as itineraries: places with
/// Trafiklab's stop ids, Trafikverket's positions where `stations` knows the
/// name. Between legs at different stops, a walk as long as the change but
/// at most `MAX_WALK`; from the station asked about to a first leg's stop
/// elsewhere (Göteborg Central to Nils Ericsonterminalen) and from a last
/// leg's to it, `END_WALK`.
fn itineraries_of(
    departures: &[Departure],
    from: DateTime<Utc>,
    until: DateTime<Utc>,
    stations: &[trafikverket::Station],
) -> Vec<Itinerary> {
    departures
        .iter()
        .filter(|d| d.legs.first().is_some_and(|l| (from..=until).contains(&l.departure_date_time.to_utc())))
        .map(|d| {
            let mut legs: Vec<Leg> = Vec::new();
            for leg in &d.legs {
                let ride = ride(leg, stations);
                match legs.last() {
                    Some(previous) if previous.to.stop_id != ride.from.stop_id => {
                        let end = ride.start_time.min(crate::checked::later_or_max(previous.end_time, MAX_WALK));
                        legs.push(walk(previous.to.clone(), ride.from.clone(), previous.end_time, end));
                    }
                    None if leg.origin.uic_station_code != d.origin.uic_station_code => {
                        let start = crate::checked::earlier_or_min(ride.start_time, END_WALK);
                        legs.push(walk(place(&d.origin, stations), ride.from.clone(), start, ride.start_time));
                    }
                    _ => {}
                }
                legs.push(ride);
            }
            if let Some(last) = legs.last().filter(|l| l.to.stop_id != Some(stop_id(&d.destination))) {
                let (from, at) = (last.to.clone(), last.end_time);
                let end = crate::checked::later_or_max(at, END_WALK);
                legs.push(walk(from, place(&d.destination, stations), at, end));
            }
            Itinerary { legs }
        })
        .collect()
}

/// Transitous's id for SJ's station: Trafiklab's, the same number.
fn stop_id(station: &SjStation) -> String {
    format!("se-Trafiklab_{}", station.uic_station_code)
}

fn place(station: &SjStation, stations: &[trafikverket::Station]) -> Place {
    let key = crate::db::station_key(&station.name);
    let position =
        stations.iter().find(|s| s.country == "SE" && crate::db::station_key(&s.name) == key).and_then(|s| s.position);
    Place {
        name: station.name.clone(),
        stop_id: Some(stop_id(station)),
        parent_id: None,
        // Not German: no DB lookups for these.
        tz: Some(SWEDISH_TIME_ZONE.to_owned()),
        track: None,
        lat: position.map(|p| p.0),
        lon: position.map(|p| p.1),
        arrival: None,
        departure: None,
        scheduled_arrival: None,
        scheduled_departure: None,
        cancelled: false,
    }
}

fn ride(leg: &SjLeg, stations: &[trafikverket::Station]) -> Leg {
    let (departs, arrives) = (leg.departure_date_time.to_utc(), leg.arrival_date_time.to_utc());
    let kind = leg.service_type.as_ref();
    let mode = match leg.transport_method.as_str() {
        "TRAIN" if kind.and_then(|k| k.modality.as_deref()) == Some("HIGHSPEED_RAIL") => "HIGHSPEED_RAIL",
        "TRAIN" => "REGIONAL_RAIL",
        "EXPRESSBUS" => "COACH",
        "TRAM" => "TRAM",
        "FERRY" => "FERRY",
        _ => "BUS",
    };
    let product = kind.and_then(|k| k.description.clone()).filter(|d| !d.is_empty());
    let mut from = place(&leg.origin, stations);
    from.scheduled_departure = Some(departs);
    from.departure = Some(departs);
    let mut to = place(&leg.destination, stations);
    to.scheduled_arrival = Some(arrives);
    to.arrival = Some(arrives);
    let run = leg.service_identifier.clone().unwrap_or_else(|| {
        format!("{}:{}", leg.service_name, leg.service_schedule_date.as_deref().unwrap_or_default())
    });
    Leg {
        mode: mode.to_owned(),
        from,
        to,
        start_time: departs,
        end_time: arrives,
        display_name: Some(product.map_or_else(|| leg.service_name.clone(), |p| format!("{p} {}", leg.service_name))),
        route_short_name: None,
        route_long_name: None,
        trip_id: Some(format!("sj:{run}")),
        // A train's number, to merge with Trafikverket's run; a bus's is a line.
        trip_short_name: (leg.transport_method == "TRAIN").then(|| leg.service_name.clone()),
        headsign: None,
        agency_name: kind.and_then(|k| k.operator_name.clone()),
        intermediate_stops: Vec::new(),
    }
}

fn walk(from: Place, to: Place, start: DateTime<Utc>, end: DateTime<Utc>) -> Leg {
    Leg {
        mode: "WALK".to_owned(),
        from,
        to,
        start_time: start,
        end_time: end.max(start),
        display_name: None,
        route_short_name: None,
        route_long_name: None,
        trip_id: None,
        trip_short_name: None,
        headsign: None,
        agency_name: None,
        intermediate_stops: Vec::new(),
    }
}

/// The dates in Sweden from `from` to `until`, at most `MAX_DAYS`.
fn dates(from: DateTime<Utc>, until: DateTime<Utc>) -> Vec<NaiveDate> {
    let last = until.with_timezone(&Stockholm).date_naive();
    from.with_timezone(&Stockholm).date_naive().iter_days().take_while(|d| *d <= last).take(MAX_DAYS).collect()
}

/// A rough route's Swedish stops: UIC code and planned departure.
type SwedishStops = Vec<(String, Option<DateTime<Utc>>)>;

/// Where SJ is asked from: a UIC code and the span its connections may leave in.
type Start = (String, DateTime<Utc>, DateTime<Utc>);

/// The Swedish ends of a search: the origin (leaving in the window) or else
/// where the rough routes enter Sweden (leaving when they do), and the
/// destination or else where they leave Sweden. The most common such stop.
/// `None` if the route doesn't pass through Sweden, or only at one stop.
pub fn swedish_ends(query: &Query, itineraries: &[Itinerary]) -> Option<(Start, String)> {
    let swedish: Vec<SwedishStops> = itineraries
        .iter()
        .map(|i| {
            i.legs
                .iter()
                .filter(|l| l.is_transit())
                .flat_map(|l| std::iter::once(&l.from).chain(&l.intermediate_stops).chain(std::iter::once(&l.to)))
                .filter_map(|p| Some((uic(p.stop_id.as_deref()?)?, p.scheduled_departure)))
                .collect()
        })
        .collect();
    let most_common = |pick: &dyn Fn(&SwedishStops) -> Option<String>| {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for code in swedish.iter().filter_map(pick) {
            let count = counts.entry(code).or_default();
            *count = count.saturating_add(1);
        }
        counts.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|(code, _)| code)
    };
    let start = if let Some(code) = uic(&query.origin.id) {
        (code, query.time, crate::checked::later_or_max(query.time, query.window))
    } else {
        let code = most_common(&|stops| stops.first().map(|s| s.0.clone()))?;
        let times = swedish.iter().flatten().filter(|s| s.0 == code).filter_map(|s| s.1);
        let (lo, hi) = times.clone().min().zip(times.max())?;
        (code, lo, hi)
    };
    let end = uic(&query.destination.id).or_else(|| most_common(&|stops| stops.last().map(|s| s.0.clone())))?;
    (start.0 != end).then_some((start, end))
}

/// SJ's connections between the search's Swedish ends, as rough routes; none
/// if the route doesn't pass through Sweden. Positions come from
/// Trafikverket's station list where there is one.
pub async fn rough(
    sj: &Sj,
    tv: Option<&trafikverket::Trafikverket>,
    query: &Query,
    itineraries: &[Itinerary],
) -> Result<Vec<Itinerary>> {
    let Some(((from, lo, hi), to)) = swedish_ends(query, itineraries) else { return Ok(Vec::new()) };
    let stations = match tv {
        Some(tv) => tv.stations().await.unwrap_or_default(),
        None => std::sync::Arc::default(),
    };
    let mut found = Vec::new();
    for date in dates(lo, hi) {
        found.extend(itineraries_of(&sj.departures(&from, &to, date).await?, lo, hi, &stations));
    }
    Ok(found)
}

#[cfg(test)]
#[path = "sj_tests.rs"]
mod tests;
