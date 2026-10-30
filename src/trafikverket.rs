//! Trafikverket's open API: announcements of every train at Swedish stations,
//! planned, forecast and actual. The source of truth for trains in Sweden, as
//! DB's boards are in Germany. A train's whole run comes in one request, so
//! its stops between the stations asked about are known too.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use tokio::sync::{OnceCell, Semaphore};

use crate::cache::DiskCache;
use crate::timetable::ProductClass;

/// The API (`--trafikverket-api` overrides it).
pub const API: &str = "https://api.trafikinfo.trafikverket.se/v2/data.json";
/// No quota, but abuse gets keys revoked: a few requests at a time.
const MAX_CONCURRENT_REQUESTS: usize = 4;
/// Attempts per request on connection resets, 429 and 5xx.
const MAX_ATTEMPTS: u32 = 3;
/// First retry delay, doubled per attempt; tests don't wait seconds.
const BACKOFF: Duration = if cfg!(test) { Duration::from_millis(1) } else { Duration::from_secs(1) };
/// Train runs asked for in one request.
const RUNS_PER_REQUEST: usize = 40;
/// The station list hardly changes: kept a week.
const STATIONS_TTL: Duration = Duration::from_hours(168);
/// Products of long-distance trains; other trains are regional,
/// commuter trains ("Pendeltåg") S-Bahn.
const LONG_DISTANCE: [&str; 10] = [
    "SJ Snabbtåg",
    "SJ InterCity",
    "SJ Regional",
    "SJ Nattåg",
    "SJ",
    "VR Snabbtåg",
    "Vy Snabbtåg",
    "Snälltåget",
    "RDC EuroNight",
    "MTRX",
];

/// A station as Trafikverket knows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Station {
    /// "G" for Göteborg C: what announcements name stations by.
    pub signature: String,
    pub name: String,
    /// "SE", "DK", …
    pub country: String,
    /// (lat, lon).
    pub position: Option<(f64, f64)>,
}

/// One run of a train: its number and the day its timetable starts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunKey {
    pub ident: String,
    /// As the API gives it ("2026-10-29T00:00:00.000+01:00"), to ask for the run again.
    pub date: String,
}

/// A train seen on a station's board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub run: RunKey,
    pub planned: DateTime<Utc>,
    pub class: ProductClass,
}

/// Arrival or departure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    pub planned: DateTime<Utc>,
    /// Actual time once passed, else the forecast, if there is one.
    pub live: Option<DateTime<Utc>>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub signature: String,
    pub arrival: Option<Moment>,
    pub departure: Option<Moment>,
    pub track: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub key: RunKey,
    /// "SJ Regional", "Öresundståg".
    pub product: Option<String>,
    pub class: ProductClass,
    /// In time order.
    pub stops: Vec<Stop>,
}

impl Run {
    /// "SJ Regional 319", or the number alone.
    pub fn label(&self) -> String {
        self.product.as_ref().map_or_else(|| self.key.ident.clone(), |product| format!("{product} {}", self.key.ident))
    }

    pub fn number(&self) -> Option<u32> {
        self.key.ident.parse().ok()
    }
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "RESPONSE")]
    response: Response,
}

#[derive(Deserialize)]
struct Response {
    #[serde(rename = "RESULT", default)]
    result: Vec<Answer>,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(rename = "TrainAnnouncement", default)]
    announcements: Vec<Announcement>,
    #[serde(rename = "TrainStation", default)]
    stations: Vec<RawStation>,
    #[serde(rename = "ERROR")]
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    #[serde(rename = "MESSAGE", default)]
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawStation {
    location_signature: String,
    advertised_location_name: String,
    #[serde(default)]
    country_code: String,
    geometry: Option<Geometry>,
}

#[derive(Deserialize)]
struct Geometry {
    #[serde(rename = "WGS84")]
    wgs84: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Described {
    #[serde(rename = "Description", default)]
    description: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Announcement {
    advertised_train_ident: String,
    scheduled_departure_date_time: String,
    location_signature: String,
    activity_type: String,
    advertised_time_at_location: DateTime<Utc>,
    estimated_time_at_location: Option<DateTime<Utc>>,
    time_at_location: Option<DateTime<Utc>>,
    #[serde(default)]
    canceled: bool,
    #[serde(default)]
    deleted: bool,
    track_at_location: Option<String>,
    #[serde(default)]
    product_information: Vec<Described>,
    #[serde(default)]
    type_of_traffic: Vec<Described>,
}

impl Announcement {
    /// Trains only: replacement buses are announced too.
    fn is_train(&self) -> bool {
        self.type_of_traffic.iter().all(|t| t.description != "Buss")
    }

    fn product(&self) -> Option<&str> {
        self.product_information.first().map(|p| p.description.as_str()).filter(|p| !p.is_empty())
    }

    fn class(&self) -> ProductClass {
        if self.type_of_traffic.iter().any(|t| t.description == "Pendeltåg") {
            ProductClass::SBahn
        } else if self.product().is_some_and(|p| LONG_DISTANCE.contains(&p)) {
            ProductClass::LongDistance
        } else {
            ProductClass::Regional
        }
    }

    fn run(&self) -> RunKey {
        RunKey { ident: self.advertised_train_ident.clone(), date: self.scheduled_departure_date_time.clone() }
    }

    fn moment(&self) -> Moment {
        Moment {
            planned: self.advertised_time_at_location,
            live: self.time_at_location.or(self.estimated_time_at_location),
            cancelled: self.canceled,
        }
    }
}

/// Fields asked for: everything announcements are turned into.
const ANNOUNCEMENT_FIELDS: [&str; 12] = [
    "AdvertisedTrainIdent",
    "ScheduledDepartureDateTime",
    "LocationSignature",
    "ActivityType",
    "AdvertisedTimeAtLocation",
    "EstimatedTimeAtLocation",
    "TimeAtLocation",
    "Canceled",
    "Deleted",
    "TrackAtLocation",
    "ProductInformation",
    "TypeOfTraffic",
];

pub struct Trafikverket {
    http: reqwest::Client,
    url: String,
    key: String,
    permits: Semaphore,
    requests: AtomicUsize,
    station_cache: DiskCache,
    stations: OnceCell<Arc<Vec<Station>>>,
}

impl Trafikverket {
    pub fn new(url: &str, key: &str, user_agent: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().user_agent(user_agent).timeout(Duration::from_secs(30)).build()?,
            url: url.to_owned(),
            key: key.to_owned(),
            permits: Semaphore::new(MAX_CONCURRENT_REQUESTS),
            requests: AtomicUsize::new(0),
            station_cache: DiskCache::new("trafikverket", STATIONS_TTL),
            stations: OnceCell::new(),
        })
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    /// The answer to one `<QUERY>`, as JSON text. Retries resets, 429 and 5xx.
    async fn post(&self, query: &str) -> Result<String> {
        let _permit = self.permits.acquire().await?;
        let body = format!("<REQUEST><LOGIN authenticationkey=\"{}\"/>{query}</REQUEST>", attribute(&self.key));
        let mut attempt: u32 = 0;
        loop {
            attempt = attempt.saturating_add(1);
            self.requests.fetch_add(1, Ordering::Relaxed);
            let sent = self.http.post(&self.url).header("Content-Type", "text/xml").body(body.clone()).send().await;
            let transient = match &sent {
                Ok(resp) => resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS || resp.status().is_server_error(),
                Err(e) => e.is_connect() || e.is_request() || e.is_timeout(),
            };
            if transient && attempt < MAX_ATTEMPTS {
                let wait = BACKOFF.saturating_mul(2_u32.saturating_pow(attempt));
                tracing::warn!(attempt, "Trafikverket: transient failure; retrying in {wait:?}");
                tokio::time::sleep(wait).await;
                continue;
            }
            let resp = sent.context("POST to Trafikverket")?;
            let status = resp.status();
            let text = resp.text().await?;
            if !status.is_success() {
                bail!("Trafikverket: {status} {}", text.chars().take(200).collect::<String>());
            }
            return Ok(text);
        }
    }

    /// Every station Trafikverket announces trains at, cached on disk for a week.
    pub async fn stations(&self) -> Result<Arc<Vec<Station>>> {
        self.stations
            .get_or_try_init(|| async {
                let query = "<QUERY objecttype=\"TrainStation\" namespace=\"rail.infrastructure\" schemaversion=\"1.5\">\
                     <FILTER><EQ name=\"Advertised\" value=\"true\"/></FILTER>\
                     <INCLUDE>LocationSignature</INCLUDE><INCLUDE>AdvertisedLocationName</INCLUDE>\
                     <INCLUDE>CountryCode</INCLUDE><INCLUDE>Geometry.WGS84</INCLUDE></QUERY>";
                let fetch = async {
                    let text = self.post(query).await?;
                    answer(&text)?;
                    Ok(text)
                };
                let text = self.station_cache.get_or_fetch(&format!("{}stations", self.url), fetch).await?;
                Ok(Arc::new(parse_stations(&text)?))
            })
            .await
            .cloned()
    }

    /// Trains (not replacement buses) at `signature` from `from` until `until`.
    pub async fn board(&self, signature: &str, from: DateTime<Utc>, until: DateTime<Utc>) -> Result<Vec<Seen>> {
        let filter = format!(
            "<EQ name=\"LocationSignature\" value=\"{}\"/><GTE name=\"AdvertisedTimeAtLocation\" value=\"{}\"/>\
             <LT name=\"AdvertisedTimeAtLocation\" value=\"{}\"/>",
            attribute(signature),
            from.to_rfc3339_opts(SecondsFormat::Secs, true),
            until.to_rfc3339_opts(SecondsFormat::Secs, true),
        );
        let announcements = parse_announcements(&self.post(&announcement_query(&filter)).await?)?;
        let mut seen: Vec<Seen> = announcements
            .iter()
            .filter(|a| a.is_train())
            .map(|a| Seen { run: a.run(), planned: a.advertised_time_at_location, class: a.class() })
            .collect();
        seen.sort_by(|a, b| a.run.cmp(&b.run).then(a.planned.cmp(&b.planned)));
        seen.dedup_by(|a, b| a.run == b.run);
        Ok(seen)
    }

    /// The whole runs of `keys`, every stop in time order.
    pub async fn runs(&self, keys: &[RunKey]) -> Result<Vec<Run>> {
        let requests = keys.chunks(RUNS_PER_REQUEST).map(|chunk| async move {
            let any = chunk.iter().fold(String::new(), |mut any, k| {
                let (ident, date) = (attribute(&k.ident), attribute(&k.date));
                // Writing to a String can't fail.
                write!(
                    any,
                    "<AND><EQ name=\"AdvertisedTrainIdent\" value=\"{ident}\"/>\
                     <EQ name=\"ScheduledDepartureDateTime\" value=\"{date}\"/></AND>"
                )
                .ok();
                any
            });
            parse_announcements(&self.post(&announcement_query(&format!("<OR>{any}</OR>"))).await?)
        });
        let mut runs = Vec::new();
        for answer in futures::future::join_all(requests).await {
            runs.extend(group_runs(answer?));
        }
        Ok(runs)
    }
}

/// Advertised announcements matching `filter`, with the fields used.
fn announcement_query(filter: &str) -> String {
    let fields = ANNOUNCEMENT_FIELDS.iter().fold(String::new(), |mut all, f| {
        write!(all, "<INCLUDE>{f}</INCLUDE>").ok();
        all
    });
    format!(
        "<QUERY objecttype=\"TrainAnnouncement\" schemaversion=\"1.9\" orderby=\"AdvertisedTimeAtLocation\">\
         <FILTER><AND><EQ name=\"Advertised\" value=\"true\"/>{filter}</AND></FILTER>{fields}</QUERY>"
    )
}

/// `text` for an XML attribute value.
fn attribute(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

fn answer(text: &str) -> Result<Answer> {
    let envelope: Envelope = serde_json::from_str(text).context("Trafikverket's answer")?;
    let answer = envelope.response.result.into_iter().next().context("Trafikverket answered nothing")?;
    if let Some(error) = &answer.error {
        bail!("Trafikverket: {}", error.message);
    }
    Ok(answer)
}

fn parse_announcements(text: &str) -> Result<Vec<Announcement>> {
    Ok(answer(text)?.announcements.into_iter().filter(|a| !a.deleted).collect())
}

fn parse_stations(text: &str) -> Result<Vec<Station>> {
    Ok(answer(text)?
        .stations
        .into_iter()
        .map(|s| Station {
            position: s.geometry.and_then(|g| g.wgs84).as_deref().and_then(point),
            signature: s.location_signature,
            name: s.advertised_location_name,
            country: s.country_code,
        })
        .collect())
}

/// "POINT (14.866 56.177)" (lon lat) as (lat, lon).
fn point(wkt: &str) -> Option<(f64, f64)> {
    let inner = wkt.trim().strip_prefix("POINT")?.trim().strip_prefix('(')?.strip_suffix(')')?;
    let mut numbers = inner.split_whitespace().map(str::parse::<f64>);
    let (lon, lat) = (numbers.next()?.ok()?, numbers.next()?.ok()?);
    numbers.next().is_none().then_some((lat, lon))
}

/// Announcements grouped into runs: per station an arrival and a departure,
/// stations in time order (an arrival before the departure at the same time).
fn group_runs(announcements: Vec<Announcement>) -> Vec<Run> {
    let mut by_run: BTreeMap<RunKey, Vec<Announcement>> = BTreeMap::new();
    for a in announcements.into_iter().filter(Announcement::is_train) {
        by_run.entry(a.run()).or_default().push(a);
    }
    by_run
        .into_iter()
        .filter_map(|(key, mut list)| {
            list.sort_by(|a, b| {
                let departs = |a: &Announcement| a.activity_type == "Avgang";
                a.advertised_time_at_location.cmp(&b.advertised_time_at_location).then(departs(a).cmp(&departs(b)))
            });
            let first = list.first()?;
            let (product, class) = (first.product().map(str::to_owned), first.class());
            let mut stops: Vec<Stop> = Vec::new();
            for a in &list {
                let departs = a.activity_type == "Avgang";
                // A new stop unless this completes the last one: its departure after its arrival.
                let completes = stops
                    .last()
                    .is_some_and(|s| s.signature == a.location_signature && departs && s.departure.is_none());
                if !completes {
                    stops.push(Stop {
                        signature: a.location_signature.clone(),
                        arrival: None,
                        departure: None,
                        track: None,
                    });
                }
                let Some(stop) = stops.last_mut() else { continue };
                if departs {
                    stop.departure = Some(a.moment());
                } else {
                    stop.arrival = Some(a.moment());
                }
                if stop.track.is_none() {
                    stop.track.clone_from(&a.track_at_location);
                }
            }
            Some(Run { key, product, class, stops })
        })
        .collect()
}

#[cfg(test)]
#[path = "trafikverket_tests.rs"]
mod tests;
