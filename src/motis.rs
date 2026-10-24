//! Minimal client for the MOTIS routing API (as served by Transitous).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use tokio::sync::Semaphore;

/// Parallel requests against the API. Transitous is volunteer-run; keep it light.
const MAX_CONCURRENT_REQUESTS: usize = 2;
/// Attempts per request when the server answers 429 Too Many Requests.
const MAX_ATTEMPTS: u32 = 4;

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
    #[serde(default)]
    pub intermediate_stops: Vec<Place>,
}

impl Leg {
    /// Walking, cycling etc. have no trip.
    pub fn is_transit(&self) -> bool {
        self.trip_id.is_some()
    }

    pub fn duration(&self) -> TimeDelta {
        self.end_time - self.start_time
    }

    pub fn label(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.route_short_name.as_deref())
            .unwrap_or(&self.mode)
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
    requests: AtomicUsize,
}

impl Motis {
    pub fn new(base: &str, user_agent: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            permits: Semaphore::new(MAX_CONCURRENT_REQUESTS),
            cache: Mutex::new(HashMap::new()),
            requests: AtomicUsize::new(0),
        })
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let _permit = self.permits.acquire().await?;
        let url = format!("{}{}", self.base, path);
        for attempt in 1.. {
            self.requests.fetch_add(1, Ordering::Relaxed);
            let resp = self
                .http
                .get(&url)
                .query(query)
                .send()
                .await
                .with_context(|| format!("GET {url}"))?;
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < MAX_ATTEMPTS {
                let wait = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()?.parse().ok())
                    .unwrap_or(1u64 << attempt);
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                continue;
            }
            if !status.is_success() {
                bail!("GET {url} failed: {status}");
            }
            return resp.json().await.with_context(|| format!("decoding {url}"));
        }
        unreachable!("loop returns or bails")
    }

    /// Resolve free text to the best-matching stop.
    pub async fn stop(&self, text: &str) -> Result<Stop> {
        let matches: Vec<GeocodeMatch> = self
            .get("/api/v1/geocode", &[("text", text.to_owned()), ("type", "STOP".to_owned())])
            .await?;
        matches
            .into_iter()
            .find(|m| m.kind == "STOP")
            .map(|m| Stop { position: m.lat.zip(m.lon), id: m.id, name: m.name })
            .with_context(|| format!("no stop found for {text:?}"))
    }

    /// Pareto-optimal itineraries departing within `[time, time + window]`.
    pub async fn plan(&self, from: &str, to: &str, time: DateTime<Utc>, window: TimeDelta) -> Result<Vec<Itinerary>> {
        let query = [
            ("fromPlace", from.to_owned()),
            ("toPlace", to.to_owned()),
            ("time", time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            ("searchWindow", window.num_seconds().max(0).to_string()),
        ];
        let key = format!("{query:?}");
        if let Some(hit) = self.cache.lock().expect("cache poisoned").get(&key) {
            return Ok(hit.clone());
        }
        let resp: PlanResponse = self.get("/api/v5/plan", &query).await?;
        self.cache
            .lock()
            .expect("cache poisoned")
            .insert(key, resp.itineraries.clone());
        Ok(resp.itineraries)
    }
}
