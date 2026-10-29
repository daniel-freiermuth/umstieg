//! Test fixtures: timetables written as small tables of stops and times.
//!
//! ```text
//! let mut b = Builder::new();
//! let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "09:02+5"), ("C", "10:00+4", "")]);
//! let tt = b.build("A", "C");
//! ```
//! Times are on 2026-10-24 in UTC; "09:02+5" is planned 09:02, actual 09:07.

#![expect(clippy::arithmetic_side_effects, reason = "fixture times")]

use std::collections::HashSet;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use proptest::prelude::*;

use crate::timetable::{Call, Footpath, Station, Time, Timetable, Trip};

/// 2026-10-24 at "HH:MM" UTC.
pub fn at(hhmm: &str) -> DateTime<Utc> {
    let (h, m) = hhmm.split_once(':').unwrap_or_else(|| panic!("bad time {hhmm:?}"));
    let (h, m): (u32, u32) = (
        h.parse().unwrap_or_else(|_| panic!("bad hour in {hhmm:?}")),
        m.parse().unwrap_or_else(|_| panic!("bad minute in {hhmm:?}")),
    );
    Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).single().unwrap_or_else(|| panic!("fixed date"))
        + TimeDelta::minutes(i64::from(h * 60 + m))
}

/// "HH:MM" (on time) or "HH:MM+N" / "HH:MM-N" (N minutes late / early); "" is none.
pub fn time(s: &str) -> Option<Time> {
    if s.is_empty() {
        return None;
    }
    let (planned, delay) = s.find(['+', '-']).map_or((s, 0), |i| {
        let (planned, delay) = s.split_at(i);
        (planned, delay.parse::<i64>().unwrap_or_else(|_| panic!("bad delay in {s:?}")))
    });
    let planned = at(planned);
    Some(Time { planned, actual: planned + TimeDelta::minutes(delay) })
}

#[derive(Default)]
pub struct Builder {
    pub stations: Vec<Station>,
    pub trips: Vec<Trip>,
    pub footpaths: Vec<Footpath>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The station named `name`, added on first use.
    pub fn station(&mut self, name: &str) -> usize {
        if let Some(i) = self.stations.iter().position(|s| s.name == name) {
            return i;
        }
        self.stations.push(Station {
            name: name.to_owned(),
            eva: None,
            tv: None,
            board_name: None,
            tz: None,
            position: None,
            motis_id: None,
        });
        self.stations.len() - 1
    }

    /// A train calling at `stops` as (station, arrival, departure); returns its index.
    /// Train numbers are the digits of the label ("ICE 774" → 774).
    pub fn trip(&mut self, label: &str, stops: &[(&str, &str, &str)]) -> usize {
        let calls = stops
            .iter()
            .map(|&(name, arrival, departure)| Call {
                station: self.station(name),
                arrival: time(arrival),
                departure: time(departure),
                platform: None,
                cancelled: false,
                from_board: true,
            })
            .collect();
        let digits: String = label.chars().filter(char::is_ascii_digit).collect();
        self.trips.push(Trip {
            label: label.to_owned(),
            numbers: digits.parse().into_iter().collect(),
            class: None,
            headsign: None,
            calls,
            transitous: false,
        });
        self.trips.len() - 1
    }

    /// Cancels `trip`'s stop at `station`.
    pub fn cancel(&mut self, trip: usize, station: &str) {
        let s = self.station(station);
        for call in self.trips[trip].calls.iter_mut().filter(|c| c.station == s) {
            call.cancelled = true;
        }
    }

    /// A walk of `minutes` from `from` to `to` (one way).
    pub fn walk(&mut self, from: &str, to: &str, minutes: i64) {
        let (from, to) = (self.station(from), self.station(to));
        self.footpaths.push(Footpath { from, to, duration: TimeDelta::minutes(minutes) });
    }

    pub fn build(mut self, origin: &str, destination: &str) -> Timetable {
        let (origin, destination) = (self.station(origin), self.station(destination));
        Timetable { stations: self.stations, trips: self.trips, footpaths: self.footpaths, origin, destination }
    }
}

/// The JSON in `<script id="{id}" type="application/json">` of a rendered page.
pub fn embedded_json(html: &str, id: &str) -> serde_json::Value {
    let open = format!("<script id=\"{id}\" type=\"application/json\">");
    let (_, rest) = html.split_once(&open).unwrap_or_else(|| panic!("no {open} in page"));
    let (json, _) = rest.split_once("</script>").unwrap_or_else(|| panic!("unclosed {open}"));
    serde_json::from_str(json.trim()).unwrap_or_else(|e| panic!("embedded JSON in {open}: {e}"))
}

/// Serves `router` on a free local port; returns its base URL ("http://127.0.0.1:port").
pub async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// How [`serve_faulty`] fails the first connections.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Closed without an answer.
    Drop,
    /// Answered with a body one byte shorter than announced, then closed.
    Truncate,
}

/// A server that closes the first `drops` connections without answering
/// (transport errors), then answers every request with `status` ("200 OK") and
/// `body`. Returns its base URL and the number of connections accepted so far.
pub async fn serve_dropping(
    drops: usize,
    status: &'static str,
    body: &'static str,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    serve_faulty(Fault::Drop, drops, status, body).await
}

/// A server that fails the first `faults` connections with `fault`, then
/// answers every request with `status` ("200 OK") and `body`. Returns its base
/// URL and the number of connections accepted so far.
pub async fn serve_faulty(
    fault: Fault,
    faults: usize,
    status: &'static str,
    body: &'static str,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let failing = counter.fetch_add(1, Ordering::SeqCst) < faults;
            if failing && fault == Fault::Drop {
                drop(socket);
                continue;
            }
            let missing = usize::from(failing);
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let length = body.len() + missing;
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}");
                socket.write_all(response.as_bytes()).await.ok();
            });
        }
    });
    (format!("http://{addr}"), accepted)
}

/// A chain's steps as "ICE 1 A-B / walk B-C / RE 2 C-D" (labels and station names).
pub fn show(tt: &Timetable, steps: &[crate::enumerate::Step]) -> String {
    use crate::enumerate::Step;
    let name = |s: usize| tt.stations[s].name.as_str();
    steps
        .iter()
        .map(|step| match *step {
            Step::Ride { trip, board, alight } => {
                let t = &tt.trips[trip];
                format!("{} {}-{}", t.label, name(t.calls[board].station), name(t.calls[alight].station))
            }
            Step::Walk { footpath } => {
                let fp = &tt.footpaths[footpath];
                format!("walk {}-{}", name(fp.from), name(fp.to))
            }
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Riding the trip labelled `label` from station `from` to station `to`.
pub fn ride(tt: &Timetable, label: &str, from: &str, to: &str) -> crate::enumerate::Step {
    let trip = tt.trips.iter().position(|t| t.label == label).unwrap_or_else(|| panic!("no trip {label}"));
    let call = |name: &str| {
        tt.trips[trip]
            .calls
            .iter()
            .position(|c| tt.stations[c.station].name == name)
            .unwrap_or_else(|| panic!("{label} doesn't call at {name}"))
    };
    crate::enumerate::Step::Ride { trip, board: call(from), alight: call(to) }
}

/// Set in the child process of [`rerun_with_env`].
const CHILD: &str = "UMSTIEG_TEST_CHILD";

/// Whether this is the child process of [`rerun_with_env`]: the test then checks
/// what depends on the environment.
pub fn in_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

/// Runs the test `path` (e.g. `db::tests::name`) alone in a child process with
/// a clean environment plus `env`, in an empty working directory, and fails
/// unless it ran and passed. Tests can't set variables themselves: they share
/// the process, and `unsafe_code` is forbidden.
pub fn rerun_with_env(path: &str, env: &[(&str, &std::ffi::OsStr)]) {
    let exe = std::env::current_exe().unwrap_or_else(|e| panic!("test binary: {e}"));
    let cwd = tempfile::tempdir().unwrap_or_else(|e| panic!("temporary directory: {e}"));
    let output = std::process::Command::new(exe)
        .args([path, "--exact", "--test-threads=1"])
        .env_clear()
        .env(CHILD, "1")
        .envs(env.iter().copied())
        .current_dir(cwd.path())
        .output()
        .unwrap_or_else(|e| panic!("running {path}: {e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{path} failed in the child:\n{stdout}\n{stderr}");
    assert!(stdout.contains("1 passed"), "{path} didn't run in the child:\n{stdout}");
}

// ---- random small timetables, for property tests ----

#[derive(Debug, Clone)]
pub struct StopSpec {
    station: usize,
    /// Minutes to the next stop.
    run: i64,
    dwell: i64,
    delay: i64,
    cancelled: bool,
    /// 0: no arrival time, 1: no departure time (stops in the middle only).
    missing: u8,
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub stations: usize,
    pub trips: Vec<(i64, Vec<StopSpec>)>,
    pub walks: Vec<(usize, usize, i64)>,
}

/// 3–5 stations, origin `S0`, destination the last; 2–10 trains of 2–5 stops
/// with delays (catching up too, but never running backwards) and up to 4
/// walks. `gaps`: stops may be cancelled or lack a time; `origin_mid_trip`:
/// trains may call at the origin after their first stop.
pub fn spec(gaps: bool, origin_mid_trip: bool) -> impl Strategy<Value = Spec> {
    (3usize..=5)
        .prop_flat_map(move |n| {
            let stop = (0..n, 0i64..=30, 0i64..=4, -5i64..=15, prop::bool::weighted(0.12), 0u8..10).prop_map(
                move |(station, run, dwell, delay, cancelled, missing)| StopSpec {
                    station,
                    run,
                    dwell,
                    delay,
                    cancelled: cancelled && gaps,
                    missing: if gaps { missing } else { u8::MAX },
                },
            );
            let trip = (0i64..=90, prop::collection::vec(stop, 2..=5));
            (Just(n), prop::collection::vec(trip, 2..=10), prop::collection::vec((0..n, 0..n, 1i64..=15), 0..=4))
        })
        .prop_map(move |(stations, trips, walks)| {
            let trips = trips
                .into_iter()
                .map(|(start, stops)| {
                    let mut seen = HashSet::new();
                    let stops: Vec<StopSpec> = stops
                        .into_iter()
                        .enumerate()
                        .filter(|(i, s)| origin_mid_trip || *i == 0 || s.station != 0)
                        .map(|(_, s)| s)
                        .filter(|s| seen.insert(s.station))
                        .collect();
                    (start, stops)
                })
                .filter(|(_, stops)| stops.len() >= 2)
                .collect();
            Spec { stations, trips, walks: walks.into_iter().filter(|&(a, b, _)| a != b).collect() }
        })
}

pub fn timetable(spec: &Spec) -> Timetable {
    let mut b = Builder::new();
    for s in 0..spec.stations {
        b.station(&format!("S{s}"));
    }
    for (k, (start, stops)) in spec.trips.iter().enumerate() {
        let mut planned = at("08:00") + TimeDelta::minutes(*start);
        let mut live = planned;
        let last = stops.len() - 1;
        let mut calls = Vec::new();
        for (i, stop) in stops.iter().enumerate() {
            let mut time = |planned: DateTime<Utc>| {
                live = live.max(planned + TimeDelta::minutes(stop.delay));
                Time { planned, actual: live }
            };
            let arrival = (i > 0).then(|| time(planned));
            planned += TimeDelta::minutes(stop.dwell);
            let departure = (i < last).then(|| time(planned));
            planned += TimeDelta::minutes(stop.run);
            let middle = i > 0 && i < last;
            calls.push(Call {
                station: stop.station,
                arrival: arrival.filter(|_| !(middle && stop.missing == 0)),
                departure: departure.filter(|_| !(middle && stop.missing == 1)),
                platform: None,
                cancelled: stop.cancelled,
                from_board: true,
            });
        }
        b.trips.push(Trip {
            label: format!("T{k}"),
            numbers: Vec::new(),
            class: None,
            headsign: None,
            calls,
            transitous: false,
        });
    }
    for &(from, to, minutes) in &spec.walks {
        b.footpaths.push(Footpath { from, to, duration: TimeDelta::minutes(minutes) });
    }
    let destination = format!("S{}", spec.stations - 1);
    b.build("S0", &destination)
}

// ---------------------------------------------------------------- Trafikverket stand-in

/// A station of the Trafikverket stand-in: (signature, name, country, lat, lon).
pub type TvStation = (&'static str, &'static str, &'static str, f64, f64);

/// A train run of the Trafikverket stand-in: number, product ("" none),
/// traffic ("Tåg", "Pendeltåg", "Buss") and stops (signature, arrival,
/// departure) in testkit times ("09:02+5"; "" none).
pub struct TvRun {
    pub ident: &'static str,
    pub product: &'static str,
    pub traffic: &'static str,
    pub stops: Vec<(&'static str, &'static str, &'static str)>,
}

/// The day every stand-in run starts, as Trafikverket writes it.
pub const TV_DATE: &str = "2026-10-24T00:00:00.000+02:00";

/// What the Trafikverket stand-in knows, and the requests it answered
/// ("stations", "board G", "runs 319,1036").
pub struct TvWorld {
    pub stations: Vec<TvStation>,
    pub runs: Vec<TvRun>,
    pub log: std::sync::Mutex<Vec<String>>,
}

impl TvWorld {
    pub fn new(stations: Vec<TvStation>, runs: Vec<TvRun>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self { stations, runs, log: std::sync::Mutex::new(Vec::new()) })
    }

    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn announcements(&self) -> Vec<(&TvRun, &'static str, &'static str, Time)> {
        self.runs
            .iter()
            .flat_map(|run| {
                run.stops.iter().flat_map(move |&(sig, arr, dep)| {
                    [("Ankomst", arr), ("Avgang", dep)]
                        .into_iter()
                        .filter_map(move |(activity, t)| Some((run, sig, activity, time(t)?)))
                })
            })
            .collect()
    }
}

/// Every `value="…"` following `name="<name>"` in a request body.
fn tv_values(body: &str, name: &str) -> Vec<String> {
    let marker = format!("name=\"{name}\" value=\"");
    body.match_indices(&marker)
        .filter_map(|(i, _)| {
            let rest = body.get(i + marker.len()..)?;
            Some(rest.split('"').next()?.to_owned())
        })
        .collect()
}

async fn tv_answer(axum::extract::State(world): axum::extract::State<std::sync::Arc<TvWorld>>, body: String) -> String {
    let note = |line: String| world.log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(line);
    if !body.contains("authenticationkey=\"key\"") {
        return r#"{"RESPONSE":{"RESULT":[{"ERROR":{"SOURCE":"Security","MESSAGE":"Invalid authentication"}}]}}"#
            .to_owned();
    }
    if body.contains("objecttype=\"TrainStation\"") {
        note("stations".to_owned());
        let stations: Vec<serde_json::Value> = world
            .stations
            .iter()
            .map(|&(sig, name, country, lat, lon)| {
                serde_json::json!({
                    "LocationSignature": sig, "AdvertisedLocationName": name, "CountryCode": country,
                    "Geometry": { "WGS84": format!("POINT ({lon} {lat})") },
                })
            })
            .collect();
        return serde_json::json!({ "RESPONSE": { "RESULT": [{ "TrainStation": stations }] } }).to_string();
    }
    let idents = tv_values(&body, "AdvertisedTrainIdent");
    let signature = tv_values(&body, "LocationSignature").into_iter().next();
    // A board's span: from the first time given, until the second.
    let span = tv_values(&body, "AdvertisedTimeAtLocation");
    let parse = |k: usize| span.get(k).and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.to_utc());
    let (from, until) = (parse(0), parse(1));
    if idents.is_empty() {
        note(format!("board {}", signature.clone().unwrap_or_default()));
    } else {
        note(format!("runs {}", idents.join(",")));
    }
    let announcements: Vec<serde_json::Value> = world
        .announcements()
        .into_iter()
        .filter(|(run, sig, _, t)| {
            if idents.is_empty() {
                signature.as_deref() == Some(*sig)
                    && from.is_none_or(|f| t.planned >= f)
                    && until.is_none_or(|u| t.planned < u)
            } else {
                idents.iter().any(|i| i == run.ident)
            }
        })
        .map(|(run, sig, activity, t)| {
            let mut a = serde_json::json!({
                "AdvertisedTrainIdent": run.ident, "ScheduledDepartureDateTime": TV_DATE,
                "LocationSignature": sig, "ActivityType": activity,
                "AdvertisedTimeAtLocation": t.planned.to_rfc3339(),
                "Canceled": false, "TrackAtLocation": "1",
                "TypeOfTraffic": [{ "Code": "x", "Description": run.traffic }],
            });
            if !run.product.is_empty() {
                a["ProductInformation"] = serde_json::json!([{ "Code": "p", "Description": run.product }]);
            }
            if t.actual != t.planned {
                a["EstimatedTimeAtLocation"] = serde_json::json!(t.actual.to_rfc3339());
            }
            a
        })
        .collect();
    serde_json::json!({ "RESPONSE": { "RESULT": [{ "TrainAnnouncement": announcements }] } }).to_string()
}

/// Serves `world` as Trafikverket's API; returns its URL. The key is "key".
pub async fn serve_trafikverket(world: &std::sync::Arc<TvWorld>) -> String {
    serve(axum::Router::new().fallback(tv_answer).with_state(world.clone())).await
}
