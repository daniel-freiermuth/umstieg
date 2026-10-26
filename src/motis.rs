//! Minimal client for the MOTIS routing API (as served by Transitous).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::cache::DiskCache;
use crate::db::fold;
use crate::sync::lock;

/// Parallel requests against the API. Transitous is volunteer-run; keep it light.
const MAX_CONCURRENT_REQUESTS: usize = 2;
/// Attempts per request on 429 Too Many Requests and transport errors.
const MAX_ATTEMPTS: u32 = 4;
/// How long plan answers are reused from disk.
const PLAN_CACHE_TTL: std::time::Duration = std::time::Duration::from_mins(10);
/// First retry delay, doubled per attempt; tests don't wait seconds.
const BACKOFF: std::time::Duration =
    if cfg!(test) { std::time::Duration::from_millis(1) } else { std::time::Duration::from_secs(1) };

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    pub name: String,
    pub stop_id: Option<String>,
    pub parent_id: Option<String>,
    pub tz: Option<String>,
    pub track: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub arrival: Option<DateTime<Utc>>,
    pub departure: Option<DateTime<Utc>>,
    pub scheduled_arrival: Option<DateTime<Utc>>,
    pub scheduled_departure: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cancelled: bool,
}

impl Place {
    /// Station-level id: the parent station if known, otherwise the stop itself.
    pub fn station_id(&self) -> Option<&str> {
        self.parent_id.as_deref().or(self.stop_id.as_deref())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Leg {
    pub mode: String,
    pub from: Place,
    pub to: Place,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub display_name: Option<String>,
    pub route_short_name: Option<String>,
    pub trip_id: Option<String>,
    pub trip_short_name: Option<String>,
    pub headsign: Option<String>,
    pub agency_name: Option<String>,
    #[serde(default)]
    pub intermediate_stops: Vec<Place>,
}

impl Leg {
    /// Walking, cycling etc. have no trip.
    pub const fn is_transit(&self) -> bool {
        self.trip_id.is_some()
    }

    pub fn duration(&self) -> TimeDelta {
        self.end_time.signed_duration_since(self.start_time)
    }

    /// "ICE 616", "RE7 (11174)", "RJ (384)", "Skånetrafiken 804 (1114)": feeds
    /// put line numbers or bare categories into the display name, so add the
    /// operator for bare numbers and the train number when it is missing.
    pub fn label(&self) -> String {
        let base = [self.display_name.as_deref(), self.route_short_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|s| !s.is_empty())
            .unwrap_or(&self.mode);
        let mut label = match &self.agency_name {
            Some(agency) if base.chars().all(|c| c.is_ascii_digit()) => format!("{agency} {base}"),
            _ => base.to_owned(),
        };
        if let Some(number) = self.train_number().map(|n| n.to_string()).filter(|n| !base.contains(n.as_str())) {
            label = format!("{label} ({number})");
        }
        label
    }

    /// Train number, e.g. 378 for "ICE 378" or "011174".
    pub fn train_number(&self) -> Option<u32> {
        let name = self.trip_short_name.as_deref()?;
        let digits = name.rsplit(|c: char| !c.is_ascii_digit()).next()?;
        digits.parse().ok().filter(|&n| n > 0)
    }

    /// DB product class: 'F' long distance, 'N' regional, 'S' S-Bahn; `None` for non-rail.
    pub fn class(&self) -> Option<char> {
        match self.mode.as_str() {
            "HIGHSPEED_RAIL" | "LONG_DISTANCE" | "NIGHT_RAIL" => Some('F'),
            "REGIONAL_FAST_RAIL" | "REGIONAL_RAIL" => Some('N'),
            "SUBURBAN" => Some('S'),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Itinerary {
    pub legs: Vec<Leg>,
}

#[derive(Deserialize)]
struct PlanResponse {
    itineraries: Vec<Itinerary>,
}

#[derive(Deserialize)]
struct GeocodeMatch {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    id: String,
    lat: Option<f64>,
    lon: Option<f64>,
    #[serde(default)]
    modes: Vec<String>,
}

impl GeocodeMatch {
    /// Whether trains call there; a coach stop may be named "… train station".
    fn has_trains(&self) -> bool {
        self.modes.iter().any(|m| m.ends_with("RAIL") || m == "LONG_DISTANCE" || m == "SUBURBAN")
    }
}

/// A stop to pick in the form: its name and Transitous id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Suggestion {
    pub name: String,
    pub id: String,
}

/// A train at a stop, from the departures of a station.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopTime {
    pub place: Place,
    pub trip_id: String,
    pub display_name: Option<String>,
    pub trip_short_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopTimesResponse {
    stop_times: Vec<StopTime>,
}

#[derive(Debug, Clone)]
pub struct Stop {
    pub id: String,
    pub name: String,
    pub position: Option<(f64, f64)>,
}

pub struct Motis {
    http: reqwest::Client,
    base: String,
    permits: Semaphore,
    cache: Mutex<HashMap<String, Vec<Itinerary>>>,
    disk: DiskCache,
    requests: AtomicUsize,
}

impl Motis {
    pub fn new(base: &str, user_agent: &str) -> Result<Self> {
        let http =
            reqwest::Client::builder().user_agent(user_agent).timeout(std::time::Duration::from_secs(60)).build()?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            permits: Semaphore::new(MAX_CONCURRENT_REQUESTS),
            cache: Mutex::new(HashMap::new()),
            disk: DiskCache::new("motis", PLAN_CACHE_TTL),
            requests: AtomicUsize::new(0),
        })
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let text = self.get_text(path, query).await?;
        serde_json::from_str(&text).with_context(|| format!("decoding {path}"))
    }

    /// Body of a GET, retrying 429 (honouring Retry-After) and transport errors.
    async fn get_text(&self, path: &str, query: &[(&str, String)]) -> Result<String> {
        let _permit = self.permits.acquire().await?;
        let url = format!("{}{}", self.base, path);
        for attempt in 1..=MAX_ATTEMPTS {
            let backoff = BACKOFF.saturating_mul(2_u32.saturating_pow(attempt));
            self.requests.fetch_add(1, Ordering::Relaxed);
            let sent_at = std::time::Instant::now();
            let sent = self.http.get(&url).query(query).send().await;
            let resp = match sent {
                Err(e) if attempt < MAX_ATTEMPTS => {
                    tracing::warn!(url, attempt, "Transitous: {e}; retrying in {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                other => other.with_context(|| format!("GET {url}"))?,
            };
            let status = resp.status();
            tracing::debug!(url, ?query, %status, attempt, elapsed = ?sent_at.elapsed(), "Transitous");
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < MAX_ATTEMPTS {
                let wait = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()?.parse().ok())
                    .map_or(backoff, std::time::Duration::from_secs);
                tracing::warn!(url, attempt, "Transitous: too many requests; retrying in {wait:?}");
                tokio::time::sleep(wait).await;
                continue;
            }
            if !status.is_success() {
                bail!("GET {url} failed: {status}");
            }
            match resp.text().await {
                Ok(text) => return Ok(text),
                Err(e) if attempt >= MAX_ATTEMPTS => return Err(e).with_context(|| format!("reading {url}")),
                Err(e) => tracing::warn!(url, attempt, "Transitous: reading the answer: {e}; retrying"),
            }
        }
        bail!("GET {url}: no attempts left")
    }

    /// Resolve free text to a stop whose name contains every place word of the
    /// text. The geocoder can rank badly ("Bad Dürkheim Hbf" → Stuttgart
    /// "Hauptbahnhof"), so station words are also tried spelled differently,
    /// and stops with trains are preferred, then stations by name over bus stops.
    pub async fn stop(&self, text: &str) -> Result<Stop> {
        const STATION_WORDS: [&str; 6] = ["hbf", "hauptbahnhof", "bahnhof", "bf", "centralstation", "station"];
        let is_station_word = |w: &str| STATION_WORDS.contains(&fold(w).as_str());
        let folded = fold(text);
        let place: Vec<&str> = folded.split(' ').filter(|w| !STATION_WORDS.contains(w)).collect();
        // Queries keep the original spelling: the geocoder doesn't know "duerkheim".
        let bare = text.split_whitespace().filter(|w| !is_station_word(w)).collect::<Vec<_>>().join(" ");
        let queries = [text.to_owned(), format!("{bare} Bahnhof"), format!("{bare} Hauptbahnhof"), bare.clone()];
        for query in &queries {
            let matches: Vec<GeocodeMatch> =
                self.get("/api/v1/geocode", &[("text", query.clone()), ("type", "STOP".to_owned())]).await?;
            let mut fitting: Vec<GeocodeMatch> = matches
                .into_iter()
                .filter(|m| m.kind == "STOP")
                .filter(|m| {
                    let name = fold(&m.name);
                    place.iter().all(|w| name.split(' ').any(|n| n == *w))
                })
                .collect();
            // Keep the geocoder's order, but stops with trains first, then stations by name.
            fitting.sort_by_key(|m| {
                (!m.has_trains(), !fold(&m.name).split(' ').any(|n| STATION_WORDS.contains(&n) || n == "c"))
            });
            if let Some(m) = fitting.into_iter().next() {
                return Ok(Stop { position: m.lat.zip(m.lon), id: m.id, name: m.name });
            }
        }
        bail!("no stop named like {text:?} found (tried: {})", queries.join(", "))
    }

    /// Stops matching `text` for the form to pick from: stops with trains
    /// first, otherwise in the geocoder's order, one per name, at most eight.
    pub async fn suggest(&self, text: &str) -> Result<Vec<Suggestion>> {
        let mut matches: Vec<GeocodeMatch> =
            self.get("/api/v1/geocode", &[("text", text.to_owned()), ("type", "STOP".to_owned())]).await?;
        matches.retain(|m| m.kind == "STOP");
        matches.sort_by_key(|m| !m.has_trains());
        let mut found: Vec<Suggestion> = Vec::new();
        for m in matches {
            if found.iter().all(|s| s.name != m.name) {
                found.push(Suggestion { name: m.name, id: m.id });
            }
        }
        found.truncate(8);
        Ok(found)
    }

    /// The stop `id` named `name`, as picked from [`Motis::suggest`]: the
    /// geocoder's answer for the name gives its position.
    pub async fn stop_by_id(&self, name: &str, id: &str) -> Result<Stop> {
        let matches: Vec<GeocodeMatch> =
            self.get("/api/v1/geocode", &[("text", name.to_owned()), ("type", "STOP".to_owned())]).await?;
        let m = matches
            .into_iter()
            .find(|m| m.id == id)
            .with_context(|| format!("Transitous no longer knows the station {name:?} ({id}); choose it again"))?;
        Ok(Stop { position: m.lat.zip(m.lon), id: m.id, name: m.name })
    }

    /// Pareto-optimal itineraries departing within `[time, time + window]`
    /// (MOTIS may pad with later ones).
    pub async fn plan(&self, from: &str, to: &str, time: DateTime<Utc>, window: TimeDelta) -> Result<Vec<Itinerary>> {
        let query = [
            ("fromPlace", from.to_owned()),
            ("toPlace", to.to_owned()),
            ("time", time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            ("searchWindow", window.num_seconds().max(0).to_string()),
        ];
        let key = format!("{query:?}");
        if let Some(hit) = lock(&self.cache).get(&key) {
            return Ok(hit.clone());
        }
        let text =
            self.disk.get_or_fetch(&format!("{}{key}", self.base), self.get_text("/api/v5/plan", &query)).await?;
        let resp: PlanResponse = serde_json::from_str(&text).context("decoding plan")?;
        lock(&self.cache).insert(key, resp.itineraries.clone());
        Ok(resp.itineraries)
    }

    /// Departures at `stop` (station-level id) from `time` on, at least `n`.
    pub async fn departures(&self, stop: &str, time: DateTime<Utc>, n: usize) -> Result<Vec<StopTime>> {
        let query = [
            ("stopId", stop.to_owned()),
            ("time", time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            ("n", n.to_string()),
        ];
        let text = self
            .disk
            .get_or_fetch(&format!("{}stoptimes{query:?}", self.base), self.get_text("/api/v5/stoptimes", &query))
            .await?;
        let resp: StopTimesResponse = serde_json::from_str(&text).context("decoding stop times")?;
        Ok(resp.stop_times)
    }

    /// A trip with all its stops, as one leg.
    pub async fn trip(&self, trip_id: &str) -> Result<Leg> {
        let query = [("tripId", trip_id.to_owned())];
        let text = self
            .disk
            .get_or_fetch(&format!("{}trip{query:?}", self.base), self.get_text("/api/v5/trip", &query))
            .await?;
        let itinerary: Itinerary = serde_json::from_str(&text).context("decoding trip")?;
        itinerary.legs.into_iter().find(Leg::is_transit).with_context(|| format!("trip {trip_id} has no ride"))
    }
}

#[cfg(test)]
#[path = "motis_tests.rs"]
mod tests;
