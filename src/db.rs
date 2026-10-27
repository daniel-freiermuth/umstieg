//! Deutsche Bahn Timetables API (DB API Marketplace): planned station boards
//! per hour plus live changes. The source of truth for trains at German stations.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use tokio::sync::Semaphore;
// tokio's clock: tests pause and advance it instead of waiting out the quota.
use tokio::time::Instant;

use crate::sync::lock;

/// The DB Timetables API (`--db-api` overrides it).
pub const API: &str = "https://apis.deutschebahn.com/db-api-marketplace/apis/timetables/v1";
/// Free plan quota: requests per `QUOTA_WINDOW`.
const REQUESTS_PER_MINUTE: usize = 60;
/// A minute; tests don't wait that long when a mistake makes them hit the quota.
const QUOTA_WINDOW: Duration = if cfg!(test) { Duration::from_millis(100) } else { Duration::from_secs(60) };
const MAX_CONCURRENT_REQUESTS: usize = 4;
/// Attempts per request on connection resets, 429 and 5xx.
const MAX_ATTEMPTS: u32 = 3;
/// First retry delay, doubled per attempt; tests don't wait seconds.
const BACKOFF: Duration = if cfg!(test) { Duration::from_millis(1) } else { Duration::from_secs(1) };
/// Station lookups, `name\teva\tdb name` per line (empty eva: DB doesn't know it).
const STATION_CACHE: &str = "stations.tsv";

pub struct Credentials {
    pub client_id: String,
    pub api_key: String,
}

impl Credentials {
    /// `DB_CLIENT_ID`/`DB_API_KEY` from the environment, else from
    /// `$XDG_CONFIG_HOME/umstieg/db.env` (`KEY=value` lines).
    pub fn load() -> Result<Option<Self>> {
        let mut text = String::new();
        if let Some(path) = xdg_dir("XDG_CONFIG_HOME", ".config").map(|d| d.join("db.env")) {
            match std::fs::read_to_string(&path) {
                Ok(read) => text = read,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
            }
        }
        Ok(Self::parse(&text, |key| std::env::var(key).ok()))
    }

    /// Credentials from `KEY=value` lines (maybe `export`ed, values maybe in
    /// double quotes), each overridden by `env(KEY)`; `None` unless both are set.
    fn parse(text: &str, env: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let mut values: HashMap<String, String> = HashMap::new();
        for line in text.lines() {
            let line = line.trim().trim_start_matches("export ");
            if let Some((k, v)) = line.split_once('=') {
                values.insert(k.trim().to_owned(), v.trim().trim_matches('"').to_owned());
            }
        }
        let get = |key: &str| env(key).or_else(|| values.get(key).cloned());
        match (get("DB_CLIENT_ID"), get("DB_API_KEY")) {
            (Some(client_id), Some(api_key)) => Some(Self { client_id, api_key }),
            _ => None,
        }
    }
}

/// `$VAR/umstieg`, falling back to `~/<fallback>/umstieg`.
fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))?;
    Some(base.join("umstieg"))
}

#[derive(Debug, Clone)]
pub struct Station {
    pub eva: String,
    pub name: String,
}

/// Arrival or departure of a train at a station.
#[derive(Debug, Clone, Default)]
pub struct Event {
    pub planned: Option<DateTime<Utc>>,
    pub changed: Option<DateTime<Utc>>,
    pub planned_platform: Option<String>,
    pub changed_platform: Option<String>,
    pub cancelled: bool,
    /// Stations before (arrival) or after (departure) this one.
    pub planned_path: Vec<String>,
    pub changed_path: Option<Vec<String>>,
}

impl Event {
    fn merge(&mut self, change: &Self) {
        self.planned = self.planned.or(change.planned);
        self.changed = change.changed.or(self.changed);
        if self.planned_platform.is_none() {
            self.planned_platform.clone_from(&change.planned_platform);
        }
        if change.changed_platform.is_some() {
            self.changed_platform.clone_from(&change.changed_platform);
        }
        self.cancelled |= change.cancelled;
        if self.planned_path.is_empty() {
            self.planned_path.clone_from(&change.planned_path);
        }
        if change.changed_path.is_some() {
            self.changed_path.clone_from(&change.changed_path);
        }
    }

    pub fn path(&self) -> &[String] {
        self.changed_path.as_deref().unwrap_or(&self.planned_path)
    }

    pub fn platform(&self) -> Option<&str> {
        self.changed_platform.as_deref().or(self.planned_platform.as_deref())
    }
}

/// One train calling at one station.
#[derive(Debug, Clone, Default)]
pub struct Visit {
    /// Identifies the train run across stations.
    pub trip: String,
    /// Position of this station in the run.
    pub seq: u32,
    pub number: Option<u32>,
    /// Product class: 'F' long distance, 'N' regional, 'S' S-Bahn. Foreign trains may lack it.
    pub class: Option<char>,
    pub category: Option<String>,
    pub line: Option<String>,
    pub arrival: Option<Event>,
    pub departure: Option<Event>,
}

impl Visit {
    fn merge(&mut self, change: &Self) {
        self.number = self.number.or(change.number);
        self.class = self.class.or(change.class);
        if self.category.is_none() {
            self.category.clone_from(&change.category);
        }
        if self.line.is_none() {
            self.line.clone_from(&change.line);
        }
        for (mine, theirs) in [(&mut self.arrival, &change.arrival), (&mut self.departure, &change.departure)] {
            if let Some(theirs) = theirs {
                mine.get_or_insert_with(Event::default).merge(theirs);
            }
        }
    }

    /// "ICE 378", "RE7 (11166)".
    pub fn label(&self) -> String {
        let number = self.number.map(|n| n.to_string()).unwrap_or_default();
        match (&self.line, &self.category) {
            // FlixTrain gives its line as "10": "FLX 1236", like "ICE 273".
            (Some(line), Some(category)) if self.number.is_some() && line.chars().all(|c| c.is_ascii_digit()) => {
                format!("{category} {number}")
            }
            (Some(line), _) => format!("{line} ({number})"),
            (None, Some(category)) => format!("{category} {number}"),
            (None, None) => number,
        }
    }
}

/// Planned visits of one station-hour.
type Board = Arc<Vec<Visit>>;

pub struct Db {
    http: reqwest::Client,
    base: String,
    credentials: Credentials,
    permits: Semaphore,
    recent: tokio::sync::Mutex<VecDeque<Instant>>,
    cache_dir: Option<PathBuf>,
    stations: Mutex<HashMap<String, Option<Station>>>,
    plans: Mutex<HashMap<(String, String), Board>>,
    changes: Mutex<HashMap<String, Arc<HashMap<String, Visit>>>>,
    requests: AtomicUsize,
    /// Time spent waiting for the per-minute quota.
    waited: Mutex<Duration>,
    /// The end of the quota wait going on, if one is.
    waiting_until: Mutex<Option<Instant>>,
}

impl Db {
    pub fn new(base: &str, credentials: Credentials, user_agent: &str) -> Result<Self> {
        let cache_dir = crate::cache::root();
        let stations = cache_dir
            .as_ref()
            .and_then(|d| std::fs::read_to_string(d.join(STATION_CACHE)).ok())
            .map(|text| parse_station_cache(&text))
            .unwrap_or_default();
        Ok(Self {
            http: reqwest::Client::builder().user_agent(user_agent).timeout(Duration::from_secs(30)).build()?,
            base: base.trim_end_matches('/').to_owned(),
            credentials,
            permits: Semaphore::new(MAX_CONCURRENT_REQUESTS),
            recent: tokio::sync::Mutex::new(VecDeque::new()),
            cache_dir,
            stations: Mutex::new(stations),
            plans: Mutex::new(HashMap::new()),
            changes: Mutex::new(HashMap::new()),
            requests: AtomicUsize::new(0),
            waited: Mutex::new(Duration::ZERO),
            waiting_until: Mutex::new(None),
        })
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    /// How long requests waited for the per-minute quota so far.
    pub fn waited(&self) -> Duration {
        *lock(&self.waited)
    }

    /// How much longer requests wait for the per-minute quota, if they do now.
    pub fn waiting(&self) -> Option<Duration> {
        let until = (*lock(&self.waiting_until))?;
        Some(until.saturating_duration_since(Instant::now()))
    }

    /// Wait until another request fits into the per-minute quota.
    async fn pace(&self) {
        let mut recent = self.recent.lock().await;
        while let Some(&oldest) = recent.front() {
            if oldest.elapsed() >= QUOTA_WINDOW {
                recent.pop_front();
            } else if recent.len() >= REQUESTS_PER_MINUTE {
                let wait = QUOTA_WINDOW.saturating_sub(oldest.elapsed());
                tracing::debug!("DB: {REQUESTS_PER_MINUTE} requests per minute reached; waiting {wait:?}");
                {
                    let mut waited = lock(&self.waited);
                    *waited = waited.saturating_add(wait);
                }
                *lock(&self.waiting_until) = Instant::now().checked_add(wait);
                tokio::time::sleep(wait).await;
                *lock(&self.waiting_until) = None;
            } else {
                break;
            }
        }
        recent.push_back(Instant::now());
    }

    /// `Ok(None)` for 404: DB has no data for that path. Retries resets, 429 and 5xx.
    async fn get(&self, path: &str) -> Result<Option<String>> {
        let _permit = self.permits.acquire().await?;
        let url = format!("{}{path}", self.base);
        let mut attempt: u32 = 0;
        loop {
            attempt = attempt.saturating_add(1);
            self.pace().await;
            self.requests.fetch_add(1, Ordering::Relaxed);
            let sent_at = Instant::now();
            let sent = self
                .http
                .get(&url)
                .header("DB-Client-Id", &self.credentials.client_id)
                .header("DB-Api-Key", &self.credentials.api_key)
                .header("Accept", "application/xml")
                .send()
                .await;
            let transient = match &sent {
                Ok(resp) => resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS || resp.status().is_server_error(),
                Err(e) => e.is_connect() || e.is_request() || e.is_timeout(),
            };
            match &sent {
                Ok(resp) => tracing::debug!(url, status = %resp.status(), attempt, elapsed = ?sent_at.elapsed(), "DB"),
                Err(e) => tracing::debug!(url, attempt, elapsed = ?sent_at.elapsed(), "DB: {e}"),
            }
            if transient && attempt < MAX_ATTEMPTS {
                let wait = BACKOFF.saturating_mul(2_u32.saturating_pow(attempt));
                tracing::warn!(url, attempt, "DB: transient failure; retrying in {wait:?}");
                tokio::time::sleep(wait).await;
                continue;
            }
            let resp = sent.with_context(|| format!("GET {url}"))?;
            let status = resp.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            let body = resp.text().await?;
            if !status.is_success() {
                bail!("GET {url} failed: {status} {}", body.chars().take(200).collect::<String>());
            }
            return Ok(Some(body));
        }
    }

    /// Map a station known by name (and maybe a foreign stop id) to a DB station.
    pub async fn resolve(&self, name: &str, stop_id: Option<&str>) -> Result<Option<Station>> {
        if let Some(hit) = lock(&self.stations).get(name) {
            return Ok(hit.clone());
        }
        let mut found = None;
        for pattern in station_patterns(name, stop_id) {
            let Some(xml) = self.get(&format!("/station/{}", urlencode(&pattern))).await? else {
                continue;
            };
            let candidates = parse_stations(&xml)?;
            // Prefer the exact name: searching "Fulda" also matches "Fulda-Horas".
            let pick = candidates
                .iter()
                .find(|s| s.name == pattern)
                .or_else(|| candidates.iter().find(|s| same_place(&s.name, name)));
            if let Some(station) = pick {
                found = Some(station.clone());
                break;
            }
        }
        lock(&self.stations).insert(name.to_owned(), found.clone());
        if let Some(dir) = &self.cache_dir {
            use std::io::Write;
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            let path = dir.join(STATION_CACHE);
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("opening {}", path.display()))?;
            let (eva, db_name) = found.as_ref().map_or(("", ""), |s| (s.eva.as_str(), s.name.as_str()));
            // One write per line: appends of concurrent searches don't interleave.
            file.write_all(format!("{name}\t{eva}\t{db_name}\n").as_bytes())
                .with_context(|| format!("writing {}", path.display()))?;
        }
        Ok(found)
    }

    /// Planned board for one hour (`slot` = "YYMMDD/HH"), cached on disk:
    /// plans for a day are fixed, changes come separately.
    async fn plan(&self, eva: &str, slot: &str) -> Result<Board> {
        let key = (eva.to_owned(), slot.to_owned());
        if let Some(hit) = lock(&self.plans).get(&key) {
            return Ok(hit.clone());
        }
        let file =
            self.cache_dir.as_ref().map(|d| d.join("db-plan").join(format!("{eva}_{}.xml", slot.replace('/', "_"))));
        let cached = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok());
        let xml = if let Some(xml) = cached {
            Some(xml)
        } else {
            let xml = self.get(&format!("/plan/{eva}/{slot}")).await?;
            if let (Some(xml), Some(file)) = (&xml, &file) {
                crate::cache::write_atomic(file, xml)?;
            }
            xml
        };
        let board = Arc::new(match xml {
            Some(xml) => parse_visits(&xml)?.into_iter().map(|(_, v)| v).collect(),
            None => Vec::new(),
        });
        lock(&self.plans).insert(key, board.clone());
        Ok(board)
    }

    /// All known changes at a station, keyed by visit id. Live; never cached on disk.
    async fn changes(&self, eva: &str) -> Result<Arc<HashMap<String, Visit>>> {
        if let Some(hit) = lock(&self.changes).get(eva) {
            return Ok(hit.clone());
        }
        let visits = Arc::new(match self.get(&format!("/fchg/{eva}")).await? {
            Some(xml) => parse_visits(&xml)?.into_iter().collect(),
            None => HashMap::new(),
        });
        lock(&self.changes).insert(eva.to_owned(), visits.clone());
        Ok(visits)
    }

    /// Trains at `eva` during the given hours, with live changes applied.
    /// Also returns which product classes the board covers per hour.
    pub async fn visits(&self, eva: &str, hours: &[DateTime<Utc>]) -> Result<(Vec<Visit>, Coverage)> {
        let changes = self.changes(eva).await?;
        let mut by_trip: HashMap<String, Visit> = HashMap::new();
        let mut coverage = Coverage::default();
        for hour in hours {
            let slot = slot(*hour);
            for visit in self.plan(eva, &slot).await?.iter() {
                if let Some(class) = visit.class {
                    coverage.0.push((slot.clone(), class));
                }
                by_trip.insert(visit_id(visit), visit.clone());
            }
        }
        for (id, change) in changes.iter() {
            match by_trip.get_mut(id) {
                Some(visit) => visit.merge(change),
                // Trains added after planning (diversions, extra trains) come with full data.
                None if change.number.is_some() => {
                    let in_hours = [&change.arrival, &change.departure]
                        .into_iter()
                        .flatten()
                        .filter_map(|e| e.planned)
                        .any(|t| hours.iter().any(|h| slot(*h) == slot(t)));
                    if in_hours {
                        by_trip.insert(id.clone(), change.clone());
                    }
                }
                None => {}
            }
        }
        coverage.0.sort();
        coverage.0.dedup();
        Ok((by_trip.into_values().collect(), coverage))
    }
}

fn parse_station_cache(text: &str) -> HashMap<String, Option<Station>> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (name, eva, db_name) = (fields.next()?, fields.next()?, fields.next()?);
            // Entries from before the rail filter may name bus stops: look those up again.
            if !eva.is_empty() && !is_rail_eva(eva) {
                return None;
            }
            let station = (!eva.is_empty()).then(|| Station { eva: eva.to_owned(), name: db_name.to_owned() });
            Some((name.to_owned(), station))
        })
        .collect()
}

/// (slot, class) pairs a fetched board contains: if a board lists trains of a
/// class for an hour, a train of that class missing from it does not call there.
#[derive(Debug, Clone, Default)]
pub struct Coverage(Vec<(String, char)>);

impl Coverage {
    pub fn covers(&self, at: DateTime<Utc>, class: char) -> bool {
        let slot = slot(at);
        self.0.iter().any(|(s, c)| *s == slot && *c == class)
    }

    /// Coverage of the given (time, class) pairs, for tests.
    #[cfg(test)]
    pub fn of(entries: &[(DateTime<Utc>, char)]) -> Self {
        Self(entries.iter().map(|&(t, c)| (slot(t), c)).collect())
    }
}

/// "YYMMDD/HH" in German local time, the API's hour slot format.
fn slot(t: DateTime<Utc>) -> String {
    t.with_timezone(&Berlin).format("%y%m%d/%H").to_string()
}

fn visit_id(v: &Visit) -> String {
    format!("{}-{}", v.trip, v.seq)
}

/// Candidate search patterns for the DB station search, best first.
fn station_patterns(name: &str, stop_id: Option<&str>) -> Vec<String> {
    let mut patterns = Vec::new();
    if let Some(eva) = stop_id.and_then(embedded_eva) {
        patterns.push(eva);
    }
    let base = name.replace(", Hauptbahnhof", " Hbf").replace(" Hauptbahnhof", " Hbf").replace(", Bahnhof", "");
    let base = base.replace("Südbahnhof", "Süd");
    let base = base.strip_suffix(" Bahnhof").unwrap_or(&base).to_owned();
    patterns.push(base.replace(" (", "(").replace(") ", ")"));
    // DB abbreviates and keeps the space after the bracket in some names:
    // "Frankfurt (Main) Flughafen Fernbahnhof" is "Frankfurt(M) Flughafen Fernbf".
    let short = base.replace(" (", "(").replace("(Main)", "(M)").replace("Fernbahnhof", "Fernbf");
    if !patterns.contains(&short) {
        patterns.push(short);
    }
    if !patterns.iter().any(|p| p == name) {
        patterns.push(name.to_owned());
    }
    patterns
}

/// Lowercase ASCII-ish form so that "Koebenhavn H" and "København H" compare equal.
pub fn fold(s: &str) -> String {
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
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Guards against wrong EVA guesses: the DB name must contain the place's first word or vice versa.
pub fn same_place(db_name: &str, place_name: &str) -> bool {
    let (db, place) = (fold(db_name), fold(place_name));
    let first = |s: &str| s.split_whitespace().next().map(str::to_owned).unwrap_or_default();
    let (db_first, place_first) = (first(&db), first(&place));
    if db_first.is_empty() || place_first.is_empty() {
        return false;
    }
    db.contains(&place_first) || place.contains(&db_first)
}

/// German and many European stop ids end in the 7-digit UIC/EVA number (80xxxxx, 86xxxxx, …).
fn embedded_eva(stop_id: &str) -> Option<String> {
    let digits: String = stop_id.rsplit(|c: char| !c.is_ascii_digit()).next()?.trim_start_matches('0').to_owned();
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

/// Rail stations have 7-digit UIC/EVA numbers (80xxxxx in Germany, 8yxxxxx
/// abroad); the station search also returns bus stops with other numbers.
fn is_rail_eva(eva: &str) -> bool {
    eva.len() == 7 && eva.starts_with('8') && eva.bytes().all(|b| b.is_ascii_digit())
}

fn parse_stations(xml: &str) -> Result<Vec<Station>> {
    let doc = roxmltree::Document::parse(xml).context("parsing station XML")?;
    Ok(doc
        .descendants()
        .filter(|n| n.has_tag_name("station"))
        .filter_map(|n| Some(Station { eva: n.attribute("eva")?.to_owned(), name: n.attribute("name")?.to_owned() }))
        .filter(|s| is_rail_eva(&s.eva))
        .collect())
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(s, "%y%m%d%H%M").ok()?;
    Berlin.from_local_datetime(&naive).earliest().map(|t| t.with_timezone(&Utc))
}

fn parse_event(n: roxmltree::Node<'_, '_>) -> Event {
    let text = |a: &str| n.attribute(a).filter(|v| !v.is_empty()).map(str::to_owned);
    let path =
        |a: &str| n.attribute(a).map(|p| p.split('|').filter(|s| !s.is_empty()).map(str::to_owned).collect::<Vec<_>>());
    Event {
        planned: n.attribute("pt").and_then(parse_time),
        changed: n.attribute("ct").and_then(parse_time),
        planned_platform: text("pp"),
        changed_platform: text("cp"),
        cancelled: n.attribute("cs") == Some("c"),
        planned_path: path("ppth").unwrap_or_default(),
        changed_path: path("cpth"),
    }
}

/// `<s>` elements of a plan or change document, keyed by their id
/// (`"<run>-<YYMMDDHHmm>-<seq>"`).
fn parse_visits(xml: &str) -> Result<Vec<(String, Visit)>> {
    let doc = roxmltree::Document::parse(xml).context("parsing timetable XML")?;
    Ok(doc
        .descendants()
        .filter(|n| n.has_tag_name("s"))
        .filter_map(|s| {
            let id = s.attribute("id")?;
            let (trip, seq) = id.rsplit_once('-')?;
            let mut visit = Visit { trip: trip.to_owned(), seq: seq.parse().ok()?, ..Visit::default() };
            for child in s.children().filter(roxmltree::Node::is_element) {
                match child.tag_name().name() {
                    "tl" => {
                        visit.number = child.attribute("n").and_then(|n| n.parse().ok());
                        visit.category = child.attribute("c").map(str::to_owned);
                        // FlixTrain and some foreign trains come without a class.
                        visit.class = child.attribute("f").and_then(|f| f.chars().next()).or_else(|| {
                            visit.category.as_deref().filter(|c| crate::timetable::is_long_distance(c)).map(|_| 'F')
                        });
                    }
                    "ar" | "dp" => {
                        if visit.line.is_none() {
                            visit.line = child.attribute("l").filter(|l| !l.is_empty()).map(str::to_owned);
                        }
                        let event = Some(parse_event(child));
                        if child.tag_name().name() == "ar" {
                            visit.arrival = event;
                        } else {
                            visit.departure = event;
                        }
                    }
                    _ => {}
                }
            }
            Some((id.to_owned(), visit))
        })
        .collect())
}

#[cfg(test)]
#[path = "db_tests.rs"]
mod tests;
