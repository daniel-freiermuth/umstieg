//! Deutsche Bahn Timetables API (DB API Marketplace): planned station boards
//! plus live changes. Used to confirm, correct or reject legs at German stations.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDateTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use tokio::sync::Semaphore;

use crate::motis::Place;

const BASE: &str = "https://apis.deutschebahn.com/db-api-marketplace/apis/timetables/v1";
/// Free plan quota.
const REQUESTS_PER_MINUTE: usize = 60;
const MAX_CONCURRENT_REQUESTS: usize = 4;
/// Transitous schedules can lag DB's by more than a few minutes; DB wins. Train
/// numbers are unique enough that the nearest match within this window is the train.
const MATCH_TOLERANCE: TimeDelta = TimeDelta::minutes(60);

pub struct Credentials {
    pub client_id: String,
    pub api_key: String,
}

impl Credentials {
    /// `DB_CLIENT_ID`/`DB_API_KEY` from the environment, else from
    /// `$XDG_CONFIG_HOME/umstieg/db.env` (`KEY=value` lines).
    pub fn load() -> Result<Option<Self>> {
        let mut values: HashMap<String, String> = HashMap::new();
        if let Some(path) = config_path() {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    for line in text.lines() {
                        let line = line.trim().trim_start_matches("export ");
                        if let Some((k, v)) = line.split_once('=') {
                            values.insert(k.trim().to_owned(), v.trim().trim_matches('"').to_owned());
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
            }
        }
        let get = |key: &str| std::env::var(key).ok().or_else(|| values.get(key).cloned());
        Ok(match (get("DB_CLIENT_ID"), get("DB_API_KEY")) {
            (Some(client_id), Some(api_key)) => Some(Self { client_id, api_key }),
            _ => None,
        })
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("umstieg").join("db.env"))
}

#[derive(Debug, Clone)]
pub struct Station {
    pub eva: String,
    pub name: String,
}

/// Arrival or departure of a train at a station.
#[derive(Debug, Clone, Default)]
struct Event {
    planned: Option<DateTime<Utc>>,
    changed: Option<DateTime<Utc>>,
    planned_platform: Option<String>,
    changed_platform: Option<String>,
    cancelled: bool,
}

impl Event {
    fn merge(&mut self, change: &Event) {
        self.planned = self.planned.or(change.planned);
        self.changed = change.changed.or(self.changed);
        self.planned_platform = self.planned_platform.take().or_else(|| change.planned_platform.clone());
        self.changed_platform = change.changed_platform.clone().or(self.changed_platform.take());
        self.cancelled |= change.cancelled;
    }
}

#[derive(Debug, Clone, Default)]
struct Visit {
    number: Option<u32>,
    /// Product class: 'F' long distance, 'N' regional, 'S' S-Bahn.
    class: Option<char>,
    arrival: Option<Event>,
    departure: Option<Event>,
}

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Arrival,
    Departure,
}

#[derive(Debug, Clone)]
pub enum Check {
    Confirmed {
        planned: DateTime<Utc>,
        actual: DateTime<Utc>,
        live: bool,
        platform: Option<String>,
    },
    Cancelled { station: String },
    NotServed { station: String },
    /// Station unknown to DB or its board is empty: no statement possible.
    NoData,
}

/// Planned visits of one station-hour, keyed by DB stop id.
type Board = Arc<Vec<(String, Visit)>>;

pub struct Db {
    http: reqwest::Client,
    credentials: Credentials,
    permits: Semaphore,
    recent: tokio::sync::Mutex<VecDeque<Instant>>,
    stations: Mutex<HashMap<String, Option<Station>>>,
    plans: tokio::sync::Mutex<HashMap<(String, String), Board>>,
    changes: tokio::sync::Mutex<HashMap<String, Arc<HashMap<String, Visit>>>>,
    requests: AtomicUsize,
}

impl Db {
    pub fn new(credentials: Credentials, user_agent: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(user_agent)
                .timeout(Duration::from_secs(30))
                .build()?,
            credentials,
            permits: Semaphore::new(MAX_CONCURRENT_REQUESTS),
            recent: tokio::sync::Mutex::new(VecDeque::new()),
            stations: Mutex::new(HashMap::new()),
            plans: tokio::sync::Mutex::new(HashMap::new()),
            changes: tokio::sync::Mutex::new(HashMap::new()),
            requests: AtomicUsize::new(0),
        })
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    /// Wait until another request fits into the per-minute quota.
    async fn pace(&self) {
        let mut recent = self.recent.lock().await;
        let minute = Duration::from_secs(60);
        while let Some(&oldest) = recent.front() {
            if oldest.elapsed() >= minute {
                recent.pop_front();
            } else if recent.len() >= REQUESTS_PER_MINUTE {
                tokio::time::sleep(minute - oldest.elapsed()).await;
            } else {
                break;
            }
        }
        recent.push_back(Instant::now());
    }

    /// `Ok(None)` for 404: DB has no data for that path.
    async fn get(&self, path: &str) -> Result<Option<String>> {
        let _permit = self.permits.acquire().await?;
        self.pace().await;
        self.requests.fetch_add(1, Ordering::Relaxed);
        let url = format!("{BASE}{path}");
        let resp = self
            .http
            .get(&url)
            .header("DB-Client-Id", &self.credentials.client_id)
            .header("DB-Api-Key", &self.credentials.api_key)
            .header("Accept", "application/xml")
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("GET {url} failed: {status} {}", body.chars().take(200).collect::<String>());
        }
        Ok(Some(body))
    }

    /// Map a MOTIS place to a DB station: EVA number embedded in the stop id, else the name.
    async fn station(&self, place: &Place) -> Result<Option<Station>> {
        if let Some(hit) = self.stations.lock().expect("cache poisoned").get(&place.name) {
            return Ok(hit.clone());
        }
        let mut found = None;
        for pattern in station_patterns(place) {
            let Some(xml) = self.get(&format!("/station/{}", urlencode(&pattern))).await? else {
                continue;
            };
            if let Some(station) = parse_stations(&xml)?.into_iter().find(|s| same_place(&s.name, &place.name)) {
                found = Some(station);
                break;
            }
        }
        self.stations
            .lock()
            .expect("cache poisoned")
            .insert(place.name.clone(), found.clone());
        Ok(found)
    }

    async fn plan(&self, eva: &str, slot: &str) -> Result<Board> {
        let key = (eva.to_owned(), slot.to_owned());
        let mut plans = self.plans.lock().await;
        if let Some(hit) = plans.get(&key) {
            return Ok(hit.clone());
        }
        let visits = match self.get(&format!("/plan/{eva}/{slot}")).await? {
            Some(xml) => parse_visits(&xml)?,
            None => Vec::new(),
        };
        let visits = Arc::new(visits);
        plans.insert(key, visits.clone());
        Ok(visits)
    }

    async fn changes(&self, eva: &str) -> Result<Arc<HashMap<String, Visit>>> {
        let mut changes = self.changes.lock().await;
        if let Some(hit) = changes.get(eva) {
            return Ok(hit.clone());
        }
        let visits = match self.get(&format!("/fchg/{eva}")).await? {
            Some(xml) => parse_visits(&xml)?.into_iter().collect(),
            None => HashMap::new(),
        };
        let visits = Arc::new(visits);
        changes.insert(eva.to_owned(), visits.clone());
        Ok(visits)
    }

    /// What DB knows about train `number` of product `class` ('F', 'N', 'S')
    /// arriving at / departing from `place` around `scheduled`.
    pub async fn check(
        &self,
        place: &Place,
        number: u32,
        class: char,
        scheduled: DateTime<Utc>,
        kind: Kind,
    ) -> Result<Check> {
        let Some(station) = self.station(place).await? else {
            return Ok(Check::NoData);
        };
        let mut slots: Vec<String> = [scheduled - MATCH_TOLERANCE, scheduled, scheduled + MATCH_TOLERANCE]
            .iter()
            .map(|t| t.with_timezone(&Berlin).format("%y%m%d/%H").to_string())
            .collect();
        slots.dedup();

        let changes = self.changes(&station.eva).await?;
        let mut visits: HashMap<String, Visit> = HashMap::new();
        for slot in &slots {
            for (id, visit) in self.plan(&station.eva, slot).await?.iter() {
                visits.insert(id.clone(), visit.clone());
            }
        }
        // Changes include trains added after planning, e.g. extra stops of a diversion.
        for (id, change) in changes.iter() {
            let visit = visits.entry(id.clone()).or_default();
            visit.number = visit.number.or(change.number);
            visit.class = visit.class.or(change.class);
            for (mine, theirs) in [(&mut visit.arrival, &change.arrival), (&mut visit.departure, &change.departure)] {
                if let Some(theirs) = theirs {
                    mine.get_or_insert_with(Event::default).merge(theirs);
                }
            }
        }
        // Some boards lack whole products (e.g. S-Bahn platforms with their own EVA).
        if !visits.values().any(|v| v.class == Some(class)) {
            return Ok(Check::NoData);
        }

        let event = visits
            .values()
            .filter(|v| v.number == Some(number))
            .filter_map(|v| {
                let event = match kind {
                    Kind::Arrival => v.arrival.as_ref(),
                    Kind::Departure => v.departure.as_ref(),
                }?;
                let planned = event.planned?;
                let off = (planned - scheduled).abs();
                (off <= MATCH_TOLERANCE).then_some((off, event, planned))
            })
            .min_by_key(|(off, _, _)| *off);
        Ok(match event {
            Some((_, event, _)) if event.cancelled => Check::Cancelled { station: station.name },
            Some((_, event, planned)) => Check::Confirmed {
                planned,
                actual: event.changed.unwrap_or(planned),
                live: event.changed.is_some(),
                platform: event.changed_platform.clone().or_else(|| event.planned_platform.clone()),
            },
            None => Check::NotServed { station: station.name },
        })
    }
}

/// Candidate search patterns for the DB station search, best first.
fn station_patterns(place: &Place) -> Vec<String> {
    let mut patterns = Vec::new();
    if let Some(eva) = place.stop_id.as_deref().and_then(embedded_eva) {
        patterns.push(eva);
    }
    let name = place.name.replace(", Hauptbahnhof", " Hbf").replace(" Hauptbahnhof", " Hbf");
    let name = name.replace(" (", "(").replace(") ", ")");
    let name = name.replace("Südbahnhof", "Süd");
    let name = name.strip_suffix(" Bahnhof").unwrap_or(&name).to_owned();
    patterns.push(name);
    if !patterns.contains(&place.name) {
        patterns.push(place.name.clone());
    }
    patterns
}

/// Lowercase ASCII-ish form so that "Koebenhavn H" and "København H" compare equal.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'ä' | 'æ' => out.push_str("ae"),
            'ö' | 'ø' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'å' => out.push_str("aa"),
            'ß' => out.push_str("ss"),
            c if c.is_alphanumeric() => out.push(c),
            _ => out.push(' '),
        }
    }
    out
}

/// Guards against wrong EVA guesses: the DB name must contain the place's first word or vice versa.
fn same_place(db_name: &str, place_name: &str) -> bool {
    let (db, place) = (fold(db_name), fold(place_name));
    let first = |s: &str| s.split_whitespace().next().map(str::to_owned).unwrap_or_default();
    let (db_first, place_first) = (first(&db), first(&place));
    !db_first.is_empty() && !place_first.is_empty() && (db.contains(&place_first) || place.contains(&db_first))
}

/// German and many European stop ids end in the 7-digit UIC/EVA number (80xxxxx, 86xxxxx, …).
fn embedded_eva(stop_id: &str) -> Option<String> {
    let digits: String = stop_id
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()?
        .trim_start_matches('0')
        .to_owned();
    (digits.len() == 7 && digits.starts_with('8')).then_some(digits)
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'(' | b')' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn parse_stations(xml: &str) -> Result<Vec<Station>> {
    let doc = roxmltree::Document::parse(xml).context("parsing station XML")?;
    Ok(doc
        .descendants()
        .filter(|n| n.has_tag_name("station"))
        .filter_map(|n| {
            Some(Station {
                eva: n.attribute("eva")?.to_owned(),
                name: n.attribute("name")?.to_owned(),
            })
        })
        .collect())
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(s, "%y%m%d%H%M").ok()?;
    Berlin.from_local_datetime(&naive).earliest().map(|t| t.with_timezone(&Utc))
}

fn parse_event(n: roxmltree::Node<'_, '_>) -> Event {
    let text = |a: &str| n.attribute(a).filter(|v| !v.is_empty()).map(str::to_owned);
    Event {
        planned: n.attribute("pt").and_then(parse_time),
        changed: n.attribute("ct").and_then(parse_time),
        planned_platform: text("pp"),
        changed_platform: text("cp"),
        cancelled: n.attribute("cs") == Some("c"),
    }
}

/// `<s>` elements of a plan or change document, keyed by stop id.
fn parse_visits(xml: &str) -> Result<Vec<(String, Visit)>> {
    let doc = roxmltree::Document::parse(xml).context("parsing timetable XML")?;
    Ok(doc
        .descendants()
        .filter(|n| n.has_tag_name("s"))
        .filter_map(|s| {
            let mut visit = Visit::default();
            for child in s.children().filter(roxmltree::Node::is_element) {
                match child.tag_name().name() {
                    "tl" => {
                        visit.number = child.attribute("n").and_then(|n| n.parse().ok());
                        visit.class = child.attribute("f").and_then(|f| f.chars().next());
                    }
                    "ar" => visit.arrival = Some(parse_event(child)),
                    "dp" => visit.departure = Some(parse_event(child)),
                    _ => {}
                }
            }
            Some((s.attribute("id")?.to_owned(), visit))
        })
        .collect())
}
