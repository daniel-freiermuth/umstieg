//! EFA (Mentz) journey planners of German transit associations, e.g. the VRN.
//! They know local trams and buses that Transitous can miss (RNV 4 Bad
//! Dürkheim → Mannheim on Saturdays). Answers are turned into MOTIS
//! itineraries, so the timetable treats them like any other rough route.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Europe::Berlin;
use serde::Deserialize;

use crate::cache::DiskCache;
use crate::motis::{Itinerary, Leg, Place};

/// Trips asked for per request; EFA answers with about this many departures.
const TRIPS_PER_REQUEST: &str = "6";
const CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const ATTEMPTS: u32 = 3;

#[derive(Deserialize)]
struct TripResponse {
    #[serde(default)]
    journeys: Vec<Journey>,
}

#[derive(Deserialize)]
struct Journey {
    #[serde(default)]
    legs: Vec<EfaLeg>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EfaLeg {
    duration: Option<i64>,
    origin: EfaStop,
    destination: EfaStop,
    transportation: Option<Transportation>,
    #[serde(default)]
    stop_sequence: Vec<EfaStop>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EfaStop {
    name: Option<String>,
    id: Option<String>,
    /// `[lat, lon]` with `coordOutputFormat=WGS84[dd.ddddd]`.
    coord: Option<Vec<f64>>,
    arrival_time_planned: Option<DateTime<Utc>>,
    arrival_time_estimated: Option<DateTime<Utc>>,
    departure_time_planned: Option<DateTime<Utc>>,
    departure_time_estimated: Option<DateTime<Utc>>,
    properties: Option<StopProperties>,
}

#[derive(Deserialize)]
struct StopProperties {
    platform: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Transportation {
    id: Option<String>,
    number: Option<String>,
    product: Option<Product>,
    destination: Option<Named>,
    properties: Option<TransportProperties>,
}

#[derive(Deserialize)]
struct Product {
    class: Option<i64>,
}

#[derive(Deserialize)]
struct Named {
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransportProperties {
    train_number: Option<String>,
    trip_code: Option<i64>,
}

pub struct Efa {
    http: reqwest::Client,
    base: String,
    disk: DiskCache,
    requests: AtomicUsize,
}

/// EFA addresses German stops by their DHID ("de:07332:116"), which DELFI
/// stop ids in Transitous carry ("de-DELFI_de:07332:116_G").
pub fn dhid(stop_id: &str) -> Option<String> {
    let id = stop_id.strip_prefix("de-DELFI_")?;
    let id = id.strip_suffix("_G").unwrap_or(id);
    id.starts_with("de:").then(|| id.to_owned())
}

impl Efa {
    pub fn new(base: &str, user_agent: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(user_agent)
                .timeout(Duration::from_secs(60))
                .build()?,
            base: base.trim_end_matches('/').to_owned(),
            disk: DiskCache::new("efa", CACHE_TTL),
            requests: AtomicUsize::new(0),
        })
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    /// Journeys from `from` to `to` (DHIDs) departing from `time` on.
    pub async fn trips(&self, from: &str, to: &str, time: DateTime<Utc>) -> Result<Vec<Itinerary>> {
        let local = time.with_timezone(&Berlin);
        let query = [
            ("outputFormat", "rapidJSON".to_owned()),
            ("coordOutputFormat", "WGS84[dd.ddddd]".to_owned()),
            ("type_origin", "any".to_owned()),
            ("name_origin", from.to_owned()),
            ("type_destination", "any".to_owned()),
            ("name_destination", to.to_owned()),
            ("itdDate", local.format("%Y%m%d").to_string()),
            ("itdTime", local.format("%H%M").to_string()),
            ("itdTripDateTimeDepArr", "dep".to_owned()),
            ("calcNumberOfTrips", TRIPS_PER_REQUEST.to_owned()),
            ("useRealtime", "1".to_owned()),
        ];
        let url = format!("{}/XML_TRIP_REQUEST2", self.base);
        let text = self.disk.get_or_fetch(&format!("{url}{query:?}"), self.get(&url, &query)).await?;
        let response: TripResponse = serde_json::from_str(&text).with_context(|| format!("decoding {url}"))?;
        Ok(response.journeys.iter().filter_map(itinerary).collect())
    }

    async fn get(&self, url: &str, query: &[(&str, String)]) -> Result<String> {
        for attempt in 1..=ATTEMPTS {
            self.requests.fetch_add(1, Ordering::Relaxed);
            let result = async {
                let resp = self.http.get(url).query(query).send().await?.error_for_status()?;
                resp.text().await
            }
            .await;
            match result {
                Ok(text) => return Ok(text),
                Err(_) if attempt < ATTEMPTS => tokio::time::sleep(Duration::from_secs(1 << attempt)).await,
                Err(e) => return Err(e).with_context(|| format!("GET {url}")),
            }
        }
        bail!("GET {url}: no attempts left")
    }
}

/// MOTIS mode for an EFA product class; `None` for walks.
fn mode(class: i64) -> Option<&'static str> {
    Some(match class {
        0 | 13 => "REGIONAL_RAIL",
        14..=16 => "HIGHSPEED_RAIL",
        1 => "SUBURBAN",
        2 => "SUBWAY",
        3 | 4 => "TRAM",
        9 => "FERRY",
        99 | 100 | 105..=107 => return None,
        _ => "BUS",
    })
}

fn place(s: &EfaStop) -> Place {
    let coord = s.coord.as_deref().filter(|c| c.len() == 2);
    Place {
        name: s.name.clone().unwrap_or_default(),
        stop_id: s.id.clone(),
        parent_id: None,
        tz: None,
        track: s.properties.as_ref().and_then(|p| p.platform.clone()).filter(|p| !p.is_empty()),
        lat: coord.map(|c| c[0]),
        lon: coord.map(|c| c[1]),
        arrival: s.arrival_time_estimated.or(s.arrival_time_planned),
        departure: s.departure_time_estimated.or(s.departure_time_planned),
        scheduled_arrival: s.arrival_time_planned,
        scheduled_departure: s.departure_time_planned,
        cancelled: false,
    }
}

fn itinerary(journey: &Journey) -> Option<Itinerary> {
    let mut legs = Vec::new();
    let mut clock: Option<DateTime<Utc>> = None;
    for leg in &journey.legs {
        let (from, to) = (place(&leg.origin), place(&leg.destination));
        let transport = leg.transportation.as_ref();
        let Some(mode) = mode(transport.and_then(|t| t.product.as_ref()?.class).unwrap_or(99)) else {
            // A walk: EFA gives its duration, times follow from the legs around it.
            let start = from.departure.or(clock)?;
            let end = start + TimeDelta::seconds(leg.duration.unwrap_or(0));
            legs.push(walk(from, to, start, end));
            clock = Some(end);
            continue;
        };
        let transport = transport?;
        let start = from.departure?;
        let end = to.arrival?;
        let properties = transport.properties.as_ref();
        let train_number = properties.and_then(|p| p.train_number.clone());
        // One id per run, shared by every journey using it, so the timetable merges them.
        let run = properties.and_then(|p| p.trip_code).unwrap_or_default();
        let day = from.scheduled_departure.unwrap_or(start).with_timezone(&Berlin).format("%Y%m%d");
        let stops = &leg.stop_sequence;
        let intermediate = if stops.len() > 2 { stops[1..stops.len() - 1].iter().map(place).collect() } else { Vec::new() };
        legs.push(Leg {
            mode: mode.to_owned(),
            from,
            to,
            start_time: start,
            end_time: end,
            display_name: transport.number.clone(),
            route_short_name: transport.number.clone(),
            trip_id: Some(format!("efa:{}:{run}:{day}", transport.id.as_deref().unwrap_or("?"))),
            trip_short_name: train_number,
            headsign: transport.destination.as_ref().and_then(|d| d.name.clone()),
            agency_name: None,
            intermediate_stops: intermediate,
        });
        clock = Some(end);
    }
    legs.iter().any(Leg::is_transit).then_some(Itinerary { legs })
}

fn walk(from: Place, to: Place, start: DateTime<Utc>, end: DateTime<Utc>) -> Leg {
    Leg {
        mode: "WALK".to_owned(),
        from,
        to,
        start_time: start,
        end_time: end,
        display_name: None,
        route_short_name: None,
        trip_id: None,
        trip_short_name: None,
        headsign: None,
        agency_name: None,
        intermediate_stops: Vec::new(),
    }
}
