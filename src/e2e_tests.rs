//! End-to-end: a whole search against local stand-ins for Transitous, EFA
//! and the DB API.
//!
//! The corridor, on Tuesday 2026-11-03 (Berlin times):
//!
//! ```text
//! Amsel ── S1 31101 08:10 → 08:40 ──────────────┐
//!       ── S1 31103 08:40 → 09:06 (live +8/+6) ─┤ Bergstadt Hbf ── ICE 101 09:10 → Kreuzhausen 09:40 → 10:30 ──┐
//!       ── STR 4 08:05 → Bergstadt Zentrum 08:41,│               ── IC 305  09:20 (departure cancelled) → 10:50 ┤ Zielhafen Hbf
//!          walk 8 min (EFA only) ───────────────┘               ── ICE 203 09:30 → Kreuzhausen → 10:45 (+4) ──┘
//! ```
//!
//! Transitous knows the S-Bahns and long-distance trains with planned times
//! only and routes the ICEs without their Kreuzhausen stop; the tram comes
//! from EFA alone; DB's boards add Kreuzhausen, the delays and the
//! cancellation. Some tests add RE 4711 (Bergstadt Hbf 09:15 → 11:10), whose
//! arrival only Transitous knows. Fictional names and EVA numbers keep the
//! per-process DB caches apart from other tests.

#![expect(clippy::arithmetic_side_effects, reason = "fixture times")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, NaiveDate, NaiveTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use clap::Parser;
use serde_json::{Value, json};

use crate::db::Credentials;
use crate::keys::Keys;
use crate::search::{self, Endpoints, Event, PlotPage, SearchArgs};
use crate::testkit;

/// The tests share the process's DB caches (plans, `stations.tsv`), which are
/// written without locking; one search at a time keeps reads from seeing
/// half-written files.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const CLIENT_ID: &str = "e2e-client";
const API_KEY: &str = "e2e-key";

// ---------------------------------------------------------------------------
// The corridor

struct Station {
    name: &'static str,
    /// Transitous (DELFI) stop id; EFA addresses it by the DHID inside.
    id: &'static str,
    eva: &'static str,
    position: (f64, f64),
}

const AMSEL: Station = Station { name: "Amsel", id: "de-DELFI_de:09001:100_G", eva: "8099001", position: (50.0, 8.0) };
const BERGSTADT: Station =
    Station { name: "Bergstadt Hbf", id: "de-DELFI_de:09002:200_G", eva: "8099002", position: (50.1, 8.3) };
const KREUZHAUSEN: Station =
    Station { name: "Kreuzhausen", id: "de-DELFI_de:09003:300_G", eva: "8099003", position: (50.3, 8.7) };
const ZIELHAFEN: Station =
    Station { name: "Zielhafen Hbf", id: "de-DELFI_de:09004:400_G", eva: "8099004", position: (50.5, 9.0) };
/// Stations Transitous's geocoder knows.
const STATIONS: [&Station; 4] = [&AMSEL, &BERGSTADT, &KREUZHAUSEN, &ZIELHAFEN];
/// Only FLX 1236 stops here, and only Transitous' trip shows it.
const MITTELSTADT: Station =
    Station { name: "Mittelstadt", id: "de-DELFI_de:09005:500_G", eva: "8099005", position: (50.2, 8.5) };

/// A DB call; times "HH:MM" planned or "HH:MM+N" with a live delay of N minutes.
struct Call {
    at: &'static Station,
    arrival: &'static str,
    departure: &'static str,
    cancelled: bool,
    /// Listed on the station's board; otherwise only named in the train's path.
    on_board: bool,
}

const fn call(at: &'static Station, arrival: &'static str, departure: &'static str) -> Call {
    Call { at, arrival, departure, cancelled: false, on_board: true }
}

/// A train run as DB's boards show it.
struct Run {
    id: &'static str,
    category: &'static str,
    number: u32,
    class: char,
    line: Option<&'static str>,
    calls: &'static [Call],
}

const DB_RUNS: &[Run] = &[
    Run {
        id: "-90031101-2611030810",
        category: "S",
        number: 31101,
        class: 'S',
        line: Some("S1"),
        calls: &[call(&AMSEL, "", "08:10"), call(&BERGSTADT, "08:40", "")],
    },
    Run {
        id: "-90031103-2611030840",
        category: "S",
        number: 31103,
        class: 'S',
        line: Some("S1"),
        calls: &[call(&AMSEL, "", "08:40+8"), call(&BERGSTADT, "09:06+6", "")],
    },
    Run {
        id: "-90000101-2611030910",
        category: "ICE",
        number: 101,
        class: 'F',
        line: None,
        calls: &[call(&BERGSTADT, "", "09:10"), call(&KREUZHAUSEN, "09:40", "09:42"), call(&ZIELHAFEN, "10:30", "")],
    },
    Run {
        id: "-90000203-2611030930",
        category: "ICE",
        number: 203,
        class: 'F',
        line: None,
        calls: &[
            call(&BERGSTADT, "", "09:30"),
            call(&KREUZHAUSEN, "09:58+4", "10:00+4"),
            call(&ZIELHAFEN, "10:45+4", ""),
        ],
    },
    Run {
        id: "-90000305-2611030920",
        category: "IC",
        number: 305,
        class: 'F',
        line: None,
        calls: &[Call { cancelled: true, ..call(&BERGSTADT, "", "09:20") }, call(&ZIELHAFEN, "10:50", "")],
    },
    // On Bergstadt's board only: DB's board at Zielhafen doesn't list its
    // arrival. Without Transitous's RE 4711 ([`StandIn::with_regional`]) a
    // train seen at one station is useless; with it, the arrival comes from
    // Transitous (a "DB/T" ride).
    Run {
        id: "-90004711-2611030915",
        category: "RE",
        number: 4711,
        class: 'N',
        line: None,
        calls: &[call(&BERGSTADT, "", "09:15"), Call { on_board: false, ..call(&ZIELHAFEN, "11:10", "") }],
    },
];

/// A `FlixTrain` only DB's boards at Bergstadt and Zielhafen list, and no
/// Transitous answer rides ([`StandIn::with_flix`]): its stops in between come
/// from Transitous' departures at Bergstadt and the trip.
const FLX_1236_RUN: Run = Run {
    id: "-90001236-2611030855",
    category: "FLX",
    number: 1236,
    class: 'F',
    line: Some("10"),
    calls: &[
        call(&BERGSTADT, "", "08:55"),
        Call { on_board: false, ..call(&MITTELSTADT, "09:20", "09:22") },
        Call { on_board: false, ..call(&KREUZHAUSEN, "09:50", "09:52") },
        call(&ZIELHAFEN, "10:40", ""),
    ],
};

/// A train as Transitous knows it: planned times, its own idea of the route.
struct Train {
    name: &'static str,
    number: &'static str,
    mode: &'static str,
    trip: &'static str,
    stops: &'static [(&'static Station, &'static str, &'static str)],
}

const S1_31101: Train = Train {
    name: "S1",
    number: "31101",
    mode: "SUBURBAN",
    trip: "20261103_08:10_s1_31101",
    stops: &[(&AMSEL, "", "08:10"), (&BERGSTADT, "08:40", "")],
};
const S1_31103: Train = Train {
    name: "S1",
    number: "31103",
    mode: "SUBURBAN",
    trip: "20261103_08:40_s1_31103",
    stops: &[(&AMSEL, "", "08:40"), (&BERGSTADT, "09:06", "")],
};
const ICE_101: Train = Train {
    name: "ICE 101",
    number: "101",
    mode: "HIGHSPEED_RAIL",
    trip: "20261103_09:10_ice_101",
    stops: &[(&BERGSTADT, "", "09:10"), (&ZIELHAFEN, "10:30", "")],
};
const ICE_203: Train = Train {
    name: "ICE 203",
    number: "203",
    mode: "HIGHSPEED_RAIL",
    trip: "20261103_09:30_ice_203",
    stops: &[(&BERGSTADT, "", "09:30"), (&ZIELHAFEN, "10:45", "")],
};
const IC_305: Train = Train {
    name: "IC 305",
    number: "305",
    mode: "LONG_DISTANCE",
    trip: "20261103_09:20_ic_305",
    stops: &[(&BERGSTADT, "", "09:20"), (&ZIELHAFEN, "10:50", "")],
};

const RE_4711: Train = Train {
    name: "RE 4711",
    number: "4711",
    mode: "REGIONAL_RAIL",
    trip: "20261103_09:15_re_4711",
    stops: &[(&BERGSTADT, "", "09:15"), (&ZIELHAFEN, "11:10", "")],
};

const FLX_1236: Train = Train {
    name: "FlixTrain FLX10",
    number: "",
    mode: "LONG_DISTANCE",
    trip: "20261103_08:55_eu-flixbus_FLX10-1236-0855",
    stops: &[
        (&BERGSTADT, "", "08:55"),
        (&MITTELSTADT, "09:20", "09:22"),
        (&KREUZHAUSEN, "09:50", "09:52"),
        (&ZIELHAFEN, "10:40", ""),
    ],
};

/// Transitous's answers per (from, to): itineraries as trains ridden end to end.
fn motis_itineraries(from: &str, to: &str, regional: bool) -> Vec<Vec<&'static Train>> {
    let mut onward = vec![vec![&ICE_101], vec![&IC_305], vec![&ICE_203]];
    if regional {
        onward.push(vec![&RE_4711]);
    }
    let answers: [(&Station, &Station, Vec<Vec<&'static Train>>); 3] = [
        (&AMSEL, &ZIELHAFEN, vec![vec![&S1_31101, &ICE_101], vec![&S1_31103, &ICE_203]]),
        (&AMSEL, &BERGSTADT, vec![vec![&S1_31101], vec![&S1_31103]]),
        (&BERGSTADT, &ZIELHAFEN, onward),
    ];
    answers.into_iter().find(|(f, t, _)| f.id == from && t.id == to).map(|(_, _, i)| i).unwrap_or_default()
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 3).unwrap()
}

/// 2026-11-03 at "HH:MM" Berlin time.
fn berlin(hhmm: &str) -> DateTime<Utc> {
    let time = NaiveTime::parse_from_str(hhmm, "%H:%M").unwrap();
    Berlin.from_local_datetime(&day().and_time(time)).single().unwrap().with_timezone(&Utc)
}

/// (planned, actual) of "HH:MM" or "HH:MM+N"; `None` for "".
fn planned_actual(s: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    if s.is_empty() {
        return None;
    }
    let (planned, delay) = s.split_once('+').map_or((s, 0), |(p, d)| (p, d.parse().unwrap()));
    let planned = berlin(planned);
    Some((planned, planned + TimeDelta::minutes(delay)))
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Transitous (MOTIS)

fn motis_place(s: &Station, arrival: &str, departure: &str) -> Value {
    let mut place = json!({
        "name": s.name, "stopId": s.id, "tz": "Europe/Berlin", "lat": s.position.0, "lon": s.position.1,
        "vertexType": "TRANSIT", "cancelled": false,
    });
    if let Some((t, _)) = planned_actual(arrival) {
        place["arrival"] = json!(iso(t));
        place["scheduledArrival"] = json!(iso(t));
    }
    if let Some((t, _)) = planned_actual(departure) {
        place["departure"] = json!(iso(t));
        place["scheduledDeparture"] = json!(iso(t));
    }
    place
}

fn motis_leg(train: &Train) -> Value {
    let (first, last) = (train.stops[0], train.stops[train.stops.len() - 1]);
    let start = planned_actual(first.2).unwrap().0;
    let end = planned_actual(last.1).unwrap().0;
    let middle = &train.stops[1..train.stops.len() - 1];
    json!({
        "mode": train.mode,
        "from": motis_place(first.0, "", first.2),
        "to": motis_place(last.0, last.1, ""),
        "duration": (end - start).num_seconds(),
        "startTime": iso(start), "endTime": iso(end),
        "scheduledStartTime": iso(start), "scheduledEndTime": iso(end),
        "realTime": false,
        "headsign": last.0.name,
        "agencyName": "DB",
        "tripId": train.trip,
        "routeShortName": train.name,
        "displayName": train.name,
        "tripShortName": train.number,
        "intermediateStops": middle.iter().map(|&(s, a, d)| motis_place(s, a, d)).collect::<Vec<_>>(),
    })
}

fn motis_itinerary(trains: &[&Train]) -> Value {
    let legs: Vec<Value> = trains.iter().map(|t| motis_leg(t)).collect();
    json!({
        "startTime": legs[0]["startTime"], "endTime": legs[legs.len() - 1]["endTime"],
        "transfers": legs.len() - 1, "legs": legs,
    })
}

fn first_departure(trains: &[&Train]) -> DateTime<Utc> {
    planned_actual(trains[0].stops[0].2).unwrap().0
}

// ---------------------------------------------------------------------------
// EFA

const EFA_ORIGIN: &str = "de:09001:100";
const EFA_HUB: &str = "de:09002:200";

fn efa_stop(name: &str, id: &str, coord: (f64, f64), arrival: &str, departure: &str) -> Value {
    let mut stop = json!({
        "name": name, "id": id, "type": "platform", "coord": [coord.0, coord.1], "properties": {"platform": "1"},
    });
    if !arrival.is_empty() {
        let t = iso(berlin(arrival));
        stop["arrivalTimePlanned"] = json!(t);
        stop["arrivalTimeEstimated"] = json!(t);
    }
    if !departure.is_empty() {
        let t = iso(berlin(departure));
        stop["departureTimePlanned"] = json!(t);
        stop["departureTimeEstimated"] = json!(t);
    }
    stop
}

/// STR 4 from the stop at Amsel station to Bergstadt Zentrum, then a walk to
/// Bergstadt Hbf (EFA names both stops differently from Transitous).
fn efa_tram_journey() -> Value {
    let origin = efa_stop("Amsel, Bahnhof", "de:09001:100:1", (50.0004, 8.0004), "", "08:05");
    let zentrum = efa_stop("Bergstadt Zentrum", "de:09002:210:1", (50.105, 8.3), "08:41", "");
    json!({"interchanges": 0, "legs": [
        {
            "duration": 2160,
            "origin": origin,
            "destination": zentrum,
            "transportation": {
                "id": "vrn:09004:4:H:j26", "name": "Straßenbahn 4", "number": "STR 4",
                "product": {"id": 3, "class": 4, "name": "Straßenbahn"},
                "destination": {"name": "Bergstadt Zentrum"},
                "properties": {"tripCode": 516},
            },
            "stopSequence": [origin, zentrum],
        },
        {
            "duration": 480,
            "origin": efa_stop("Bergstadt Zentrum", "de:09002:210:1", (50.105, 8.3), "", ""),
            "destination": efa_stop("Bergstadt, Hauptbahnhof", "de:09002:200", (50.1002, 8.3002), "", ""),
            "transportation": {"product": {"class": 100, "name": "Fußweg"}},
        },
    ]})
}

// ---------------------------------------------------------------------------
// DB Timetables API

fn db_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Berlin).format("%y%m%d%H%M").to_string()
}

fn slot(t: DateTime<Utc>) -> String {
    t.with_timezone(&Berlin).format("%y%m%d/%H").to_string()
}

/// The planned `<s>` of `run` at its `k`-th call.
fn planned_visit(run: &Run, k: usize) -> String {
    let c = &run.calls[k];
    let line = run.line.map(|l| format!(" l=\"{l}\"")).unwrap_or_default();
    let path = |calls: &[Call]| calls.iter().map(|c| c.at.name).collect::<Vec<_>>().join("|");
    let event = |tag: &str, time: &str, path: String| {
        planned_actual(time).map(|(t, _)| format!("<{tag} pt=\"{}\" pp=\"1\"{line} ppth=\"{path}\"/>", db_time(t)))
    };
    format!(
        "<s id=\"{}-{}\"><tl f=\"{}\" t=\"p\" o=\"80\" c=\"{}\" n=\"{}\"/>{}{}</s>",
        run.id,
        k + 1,
        run.class,
        run.category,
        run.number,
        event("ar", c.arrival, path(&run.calls[..k])).unwrap_or_default(),
        event("dp", c.departure, path(&run.calls[k + 1..])).unwrap_or_default(),
    )
}

/// The live changes of `run` at its `k`-th call, if any.
fn changed_visit(run: &Run, k: usize) -> Option<String> {
    let c = &run.calls[k];
    let event = |tag: &str, time: &str| {
        let (planned, actual) = planned_actual(time)?;
        let ct = if actual == planned { String::new() } else { format!(" ct=\"{}\"", db_time(actual)) };
        let cs = if c.cancelled { " cs=\"c\"" } else { "" };
        (!ct.is_empty() || !cs.is_empty()).then(|| format!("<{tag}{ct}{cs}/>"))
    };
    let events: String = [event("ar", c.arrival), event("dp", c.departure)].into_iter().flatten().collect();
    (!events.is_empty()).then(|| format!("<s id=\"{}-{}\" eva=\"{}\">{events}</s>", run.id, k + 1, c.at.eva))
}

/// Board entries at `eva`: (run, index of the call).
fn calls_at(eva: &str, flix: bool) -> impl Iterator<Item = (&'static Run, usize)> + '_ {
    let extra: &[Run] = if flix { std::slice::from_ref(&FLX_1236_RUN) } else { &[] };
    DB_RUNS
        .iter()
        .chain(extra)
        .flat_map(|run| (0..run.calls.len()).map(move |k| (run, k)))
        .filter(move |(r, k)| r.calls[*k].at.eva == eva && r.calls[*k].on_board)
}

// ---------------------------------------------------------------------------
// The stand-in servers

#[derive(Clone, Copy, PartialEq, Eq)]
enum Efa {
    Working,
    /// Answers 200 with an HTML maintenance page.
    Broken,
    /// Not configured (`--efa ''`).
    Off,
}

struct World {
    efa: Efa,
    /// Serve RE 4711 too.
    regional: bool,
    /// Serve FLX 1236 too.
    flix: bool,
    /// Requests received, as "motis …", "efa …" and "db …" lines.
    log: Mutex<Vec<String>>,
}

impl World {
    fn note(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }
}

struct StandIn {
    base: String,
    world: Arc<World>,
}

impl StandIn {
    async fn start(efa: Efa) -> Self {
        Self::launch(efa, false, false).await
    }

    /// The corridor plus RE 4711.
    async fn with_regional() -> Self {
        Self::launch(Efa::Working, true, false).await
    }

    /// The corridor plus FLX 1236.
    async fn with_flix() -> Self {
        Self::launch(Efa::Working, false, true).await
    }

    /// Every stand-in gets its own path prefix: the MOTIS and EFA disk caches
    /// are keyed by URL, and ports get reused. The DB plan cache is keyed by
    /// EVA and hour alone, and boards differ between worlds (FLX 1236), so
    /// the corridor's cached plans go.
    async fn launch(efa: Efa, regional: bool, flix: bool) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        if let Some(Ok(plans)) = crate::cache::root().map(|root| std::fs::read_dir(root.join("db-plan"))) {
            let corridor = [&AMSEL, &BERGSTADT, &KREUZHAUSEN, &ZIELHAFEN, &MITTELSTADT].map(|s| format!("{}_", s.eva));
            for file in plans.flatten() {
                if corridor.iter().any(|eva| file.file_name().to_string_lossy().starts_with(eva.as_str())) {
                    std::fs::remove_file(file.path()).ok();
                }
            }
        }
        let world = Arc::new(World { efa, regional, flix, log: Mutex::new(Vec::new()) });
        let router = Router::new()
            .route("/{scope}/api/v1/geocode", get(geocode))
            .route("/{scope}/api/v5/plan", get(plan))
            .route("/{scope}/api/v5/stoptimes", get(stoptimes))
            .route("/{scope}/api/v5/trip", get(trip))
            .route("/{scope}/XML_TRIP_REQUEST2", get(efa_trips))
            .route("/{scope}/db/station/{pattern}", get(db_station))
            .route("/{scope}/db/plan/{eva}/{date}/{hour}", get(db_plan))
            .route("/{scope}/db/fchg/{eva}", get(db_changes))
            .with_state(world.clone());
        let url = testkit::serve(router).await;
        Self { base: format!("{url}/e2e{}", NEXT.fetch_add(1, Ordering::Relaxed)), world }
    }

    fn endpoints(&self) -> Endpoints {
        Endpoints {
            efa: if self.world.efa == Efa::Off { String::new() } else { self.base.clone() },
            api: self.base.clone(),
            db_api: format!("{}/db", self.base),
            trafikverket_api: format!("{}/trafikverket", self.base),
            // Not asked: no Swedish stops here (a request would 404 and warn).
            sj: format!("{}/sj", self.base),
            contact: "e2e@example.org".to_owned(),
        }
    }

    fn requests(&self, service: &str) -> Vec<String> {
        let prefix = format!("{service} ");
        self.world.log.lock().unwrap().iter().filter(|l| l.starts_with(&prefix)).cloned().collect()
    }
}

type Params = Query<HashMap<String, String>>;

async fn geocode(State(world): State<Arc<World>>, Query(q): Params) -> Response {
    let text = q.get("text").cloned().unwrap_or_default().to_lowercase();
    world.note(format!("motis geocode {text}"));
    let words: Vec<&str> =
        text.split_whitespace().filter(|w| !["hbf", "bahnhof", "hauptbahnhof"].contains(w)).collect();
    let matches: Vec<Value> = STATIONS
        .iter()
        .filter(|s| !words.is_empty() && words.iter().all(|w| s.name.to_lowercase().contains(w)))
        .map(|s| json!({"type": "STOP", "name": s.name, "id": s.id, "lat": s.position.0, "lon": s.position.1}))
        .collect();
    axum::Json(matches).into_response()
}

/// Itineraries whose first train leaves in `[time, time + searchWindow]`.
async fn plan(State(world): State<Arc<World>>, Query(q): Params) -> Response {
    let (from, to) = (&q["fromPlace"], &q["toPlace"]);
    let time: DateTime<Utc> = q["time"].parse().unwrap();
    let window = TimeDelta::seconds(q["searchWindow"].parse().unwrap());
    world.note(format!("motis plan {from} {to} {} {}", q["time"], q["searchWindow"]));
    let itineraries: Vec<Value> = motis_itineraries(from, to, world.regional)
        .iter()
        .filter(|trains| (time..=time + window).contains(&first_departure(trains)))
        .map(|trains| motis_itinerary(trains))
        .collect();
    axum::Json(json!({"itineraries": itineraries})).into_response()
}

/// Departures at a stop: FLX 1236 at Bergstadt, and the ICE 101 next to it.
async fn stoptimes(State(world): State<Arc<World>>, Query(q): Params) -> Response {
    world.note(format!("motis stoptimes {} {}", q["stopId"], q["time"]));
    let at_bergstadt = |train: &Train| {
        let leg = motis_leg(train);
        json!({ "place": leg["from"], "mode": train.mode, "tripId": train.trip,
                "displayName": train.name, "tripShortName": train.number })
    };
    let stop_times = if world.flix && q["stopId"] == BERGSTADT.id {
        vec![at_bergstadt(&FLX_1236), at_bergstadt(&ICE_101)]
    } else {
        Vec::new()
    };
    axum::Json(json!({ "stopTimes": stop_times })).into_response()
}

async fn trip(State(world): State<Arc<World>>, Query(q): Params) -> Response {
    world.note(format!("motis trip {}", q["tripId"]));
    [&FLX_1236, &ICE_101].into_iter().find(|t| t.trip == q["tripId"]).map_or_else(
        || StatusCode::NOT_FOUND.into_response(),
        |train| axum::Json(json!({ "legs": [motis_leg(train)] })).into_response(),
    )
}

async fn efa_trips(State(world): State<Arc<World>>, Query(q): Params) -> Response {
    let (from, to) = (&q["name_origin"], &q["name_destination"]);
    world.note(format!("efa {from} {to} {} {}", q["itdDate"], q["itdTime"]));
    if world.efa == Efa::Broken {
        return "<html><body>Wartungsarbeiten</body></html>".into_response();
    }
    assert_eq!(q["outputFormat"], "rapidJSON");
    let asked = NaiveTime::parse_from_str(&q["itdTime"], "%H%M").unwrap();
    let tram_runs = q["itdDate"] == day().format("%Y%m%d").to_string()
        && asked <= NaiveTime::from_hms_opt(8, 5, 0).unwrap()
        && (from.as_str(), to.as_str()) == (EFA_ORIGIN, EFA_HUB);
    let journeys = if tram_runs { vec![efa_tram_journey()] } else { Vec::new() };
    axum::Json(json!({"version": "10.6", "journeys": journeys})).into_response()
}

fn xml(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/xml")], body).into_response()
}

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("DB-Client-Id").is_some_and(|v| v == CLIENT_ID)
        && headers.get("DB-Api-Key").is_some_and(|v| v == API_KEY)
}

async fn db_station(
    State(world): State<Arc<World>>,
    headers: HeaderMap,
    Path((_, pattern)): Path<(String, String)>,
) -> Response {
    world.note(format!("db station {pattern}"));
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let extra: &[Run] = if world.flix { std::slice::from_ref(&FLX_1236_RUN) } else { &[] };
    let found: String = DB_RUNS
        .iter()
        .chain(extra)
        .flat_map(|r| r.calls.iter().map(|c| c.at))
        .filter(|s| s.name.to_lowercase().contains(&pattern.to_lowercase()) || s.eva == pattern)
        .map(|s| format!("<station name=\"{}\" eva=\"{}\" ds100=\"X{}\"/>", s.name, s.eva, s.eva))
        .fold(Vec::<String>::new(), |mut all, s| {
            if !all.contains(&s) {
                all.push(s);
            }
            all
        })
        .concat();
    xml(format!("<?xml version='1.0' encoding='UTF-8'?><stations>{found}</stations>"))
}

/// Visits planned in the hour, by departure (arrival at the last stop).
async fn db_plan(
    State(world): State<Arc<World>>,
    headers: HeaderMap,
    Path((_, eva, date, hour)): Path<(String, String, String, String)>,
) -> Response {
    world.note(format!("db plan {eva} {date}/{hour}"));
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let wanted = format!("{date}/{hour}");
    let visits: String = calls_at(&eva, world.flix)
        .filter(|(run, k)| {
            let c = &run.calls[*k];
            let t = planned_actual(c.departure).or_else(|| planned_actual(c.arrival)).unwrap().0;
            slot(t) == wanted
        })
        .map(|(run, k)| planned_visit(run, k))
        .collect();
    xml(format!("<?xml version='1.0' encoding='UTF-8'?><timetable station='{eva}'>{visits}</timetable>"))
}

async fn db_changes(
    State(world): State<Arc<World>>,
    headers: HeaderMap,
    Path((_, eva)): Path<(String, String)>,
) -> Response {
    world.note(format!("db fchg {eva}"));
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let changes: String = calls_at(&eva, world.flix).filter_map(|(run, k)| changed_visit(run, k)).collect();
    xml(format!("<?xml version='1.0' encoding='UTF-8'?><timetable station='{eva}'>{changes}</timetable>"))
}

// ---------------------------------------------------------------------------
// Running searches

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: SearchArgs,
}

/// The searches' start, 08:00 Berlin, as "--at".
fn start_at() -> String {
    at("08:00")
}

/// "--at" for `hhmm` Berlin, the searches' --time-zone.
fn at(hhmm: &str) -> String {
    format!("{} {hhmm}", day())
}

/// Amsel → Zielhafen Hbf from 08:00 for an hour, every connection listed,
/// unless `extra` says otherwise.
/// Amsel → Zielhafen Hbf from 08:00 Berlin time for an hour, every connection
/// listed, unless `extra` says otherwise.
fn args(extra: &[&str]) -> SearchArgs {
    let start = start_at();
    let mut argv = vec!["umstieg", "Amsel", "Zielhafen Hbf"];
    let defaults =
        [("--at", start.as_str()), ("--time-zone", "Europe/Berlin"), ("--window", "60"), ("--per-group", "0")];
    for (option, default) in defaults {
        if !extra.contains(&option) {
            argv.extend([option, default]);
        }
    }
    // Every connection, so tests see what the enumeration finds; listing
    // only the best is tested on its own.
    argv.push("--all");
    argv.extend(extra);
    Cli::try_parse_from(argv).unwrap().args
}

fn credentials() -> Credentials {
    Credentials { client_id: CLIENT_ID.to_owned(), api_key: API_KEY.to_owned() }
}

/// DB's credentials only: the stand-in world is German.
fn keys(with_credentials: bool) -> Keys {
    Keys { db: with_credentials.then(credentials), trafikverket: None }
}

#[expect(clippy::unnecessary_wraps, reason = "the signature serve expects of a key loader")]
fn fake_keys() -> anyhow::Result<Keys> {
    Ok(keys(true))
}

struct Searched {
    text: String,
    html: Option<String>,
    warnings: Vec<String>,
}

async fn search(stand_in: &StandIn, extra: &[&str], with_credentials: bool, plot: bool) -> anyhow::Result<Searched> {
    let warnings = Mutex::new(Vec::new());
    let events = |event: Event<'_>| {
        if let Event::Warning(w) = event {
            warnings.lock().unwrap().push(w.to_owned());
        }
    };
    let outcome = search::run(
        &args(extra),
        &stand_in.endpoints(),
        keys(with_credentials),
        plot.then_some(PlotPage { back: None, details_url: None }),
        &events,
    )
    .await?;
    Ok(Searched { text: outcome.text, html: outcome.plot.map(|p| p.html), warnings: warnings.into_inner().unwrap() })
}

/// Group headers and connection lines of a report.
fn listing(text: &str) -> Vec<&str> {
    text.lines().filter(|l| l.starts_with('■') || is_connection(l)).collect()
}

fn summary(text: &str) -> &str {
    text.lines().find(|l| l.starts_with("fastest ")).unwrap_or_else(|| panic!("no summary in\n{text}"))
}

/// "  08:10 CET → 10:30 CET …", not the indented data credits.
fn is_connection(line: &str) -> bool {
    line.strip_prefix("  ").is_some_and(|l| l.starts_with(|c: char| c.is_ascii_digit()))
}

fn connection_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|l| is_connection(l)).collect()
}

// ---------------------------------------------------------------------------
// Searches through search::run

#[tokio::test(flavor = "multi_thread")]
async fn live_search_lists_every_connection_with_delays_and_markers() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let found = search(&stand_in, &[], true, false).await.unwrap();
    let text = &found.text;

    assert!(found.warnings.is_empty(), "{:?}", found.warnings);
    assert!(text.starts_with("Amsel → Zielhafen Hbf, departing "), "{text}");
    assert!(text.contains("rough routes via Bergstadt Hbf ·"), "{text}");
    assert!(text.contains("\nstations found along trains' routes: Kreuzhausen\n"), "{text}");
    assert!(
        summary(text).starts_with(
            "fastest 2h01 · limits: ≤ 5h01 travel time, ≤ 15 changes · 6 connections in 6 groups, \
             1 with a change missed by ≤ 5m ("
        ),
        "{text}"
    );
    let expected = [
        "■ S1 (31101) … ICE 101  (1)",
        "  08:10 CET → 10:30 CET  2h20  1×  min 30m   08:10 CET S1 (31101) → 08:40 CET Bergstadt Hbf ‖30m‖ 09:10 CET ICE 101 → 10:30 CET Zielhafen Hbf",
        "■ STR 4 … ICE 101  (1)",
        "  08:05 CET → 10:30 CET  2h25  1×  min 21m   08:05 CET STR 4 → 08:41 CET Bergstadt Zentrum ‖21m walk 8m‖ \
         09:10 CET Bergstadt Hbf ICE 101 → 10:30 CET Zielhafen Hbf",
        "■ S1 (31101) … ICE 203  (1)",
        "  08:10 CET → 10:49 CET  2h39  1×  min 50m   08:10 CET S1 (31101) → 08:40 CET Bergstadt Hbf ‖50m‖ 09:30 CET ICE 203 → 10:49 CET (+4) Zielhafen Hbf",
        "■ STR 4 … ICE 203  (1)",
        "  08:05 CET → 10:49 CET  2h44  1×  min 41m   08:05 CET STR 4 → 08:41 CET Bergstadt Zentrum ‖41m walk 8m‖ \
         09:30 CET Bergstadt Hbf ICE 203 → 10:49 CET (+4) Zielhafen Hbf",
        "■ S1 (31103) … ICE 203  (1)",
        "  08:48 CET → 10:49 CET  2h01  1×  min 18m   08:48 CET (+8) S1 (31103) → 09:12 CET (+6) Bergstadt Hbf ‖18m‖ 09:30 CET ICE 203 → 10:49 CET (+4) Zielhafen Hbf",
        "■ S1 (31103) … ICE 101  (1)",
        "  08:48 CET → 10:30 CET  1h42  1×  min missed 2m✗   08:48 CET (+8) S1 (31103) → 09:12 CET (+6) Bergstadt Hbf ‖missed 2m✗‖ 09:10 CET ICE 101 → 10:30 CET Zielhafen Hbf",
    ];
    assert_eq!(listing(text), expected, "{text}");
    assert!(text.ends_with("live data where available.\n      Deutsche Bahn Timetables API.\n"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn trains_only_db_boards_know_get_their_stops_from_transitous() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::with_flix().await;
    let found = search(&stand_in, &[], true, true).await.unwrap();
    assert!(found.warnings.is_empty(), "{:?}", found.warnings);
    let flx = "08:55 CET FLX 1236 → 10:40 CET Zielhafen Hbf";
    assert!(found.text.contains(flx), "{}", found.text);
    // In the plot the ride passes the stops only Transitous' trip has.
    let data = testkit::embedded_json(found.html.as_deref().unwrap(), "data");
    let names = data["names"].as_array().unwrap();
    let ride = data["pieces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["info"].as_str().unwrap().starts_with("FLX 1236"))
        .unwrap();
    let stops: Vec<&str> = ride["pts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| names[usize::try_from(p[1].as_u64().unwrap()).unwrap()].as_str().unwrap())
        .collect();
    assert_eq!(stops, ["Bergstadt Hbf", "Mittelstadt", "Mittelstadt", "Kreuzhausen", "Kreuzhausen", "Zielhafen Hbf"]);
    // Two requests: the departures at Bergstadt, then the trip.
    let asked: Vec<String> = stand_in
        .requests("motis")
        .into_iter()
        .filter(|r| r.starts_with("motis stoptimes") || r.starts_with("motis trip"))
        .collect();
    assert_eq!(
        asked,
        [format!("motis stoptimes {} {}", BERGSTADT.id, iso(berlin("08:53"))), format!("motis trip {}", FLX_1236.trip)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn detours_are_pruned_unless_kept() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let detour = "  08:10 CET → 10:49 CET  2h39  2×  min 24m   08:10 CET S1 (31101) → 08:40 CET Bergstadt Hbf ‖30m‖ 09:10 CET ICE 101 \
                  → 09:40 CET Kreuzhausen ‖24m‖ 10:04 CET (+4) ICE 203 → 10:49 CET (+4) Zielhafen Hbf";
    let pruned = search(&stand_in, &[], true, false).await.unwrap();
    assert!(!pruned.text.contains("09:40 CET Kreuzhausen"), "{}", pruned.text);

    let kept = search(&stand_in, &["--keep-detours"], true, false).await.unwrap();
    let text = &kept.text;
    // The detour after the missed change misses it too.
    assert!(summary(text).contains(" · 9 connections in 6 groups, 2 with a change missed"), "{text}");
    // Same arrival, one change more: after the direct connection of its group.
    let lines = listing(text);
    let group = lines.iter().position(|l| *l == "■ S1 (31101) … ICE 203  (2)").unwrap_or_else(|| panic!("{text}"));
    assert!(lines[group + 1].ends_with("‖50m‖ 09:30 CET ICE 203 → 10:49 CET (+4) Zielhafen Hbf"), "{text}");
    assert_eq!(lines[group + 2], detour);
    // A detour after a missed change is listed with it, sorted after the feasible one.
    let group = lines.iter().position(|l| *l == "■ S1 (31103) … ICE 203  (2)").unwrap_or_else(|| panic!("{text}"));
    assert!(lines[group + 1].contains("‖18m‖"), "{text}");
    assert!(lines[group + 2].contains("‖missed 2m✗‖ 09:10 CET ICE 101 → 09:40 CET Kreuzhausen ‖24m‖"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn missed_by_bounds_the_listed_missed_changes() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    // S1 31103 reaches Bergstadt at 09:12, ICE 101 leaves at 09:10.
    for (missed_by, listed) in [("0", false), ("1", false), ("2", true), ("5", true)] {
        let found = search(&stand_in, &["--missed-by", missed_by], true, false).await.unwrap();
        let text = &found.text;
        assert_eq!(text.contains("‖missed 2m✗‖ 09:10 CET ICE 101"), listed, "--missed-by {missed_by}:\n{text}");
        let counts = if listed {
            format!("6 connections in 6 groups, 1 with a change missed by ≤ {missed_by}m (")
        } else {
            "5 connections in 5 groups (".to_owned()
        };
        assert!(summary(text).contains(&counts), "--missed-by {missed_by}:\n{text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn planned_ignores_delays_but_keeps_cancellations() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    for (min_transfer, buffer) in [("5", "4m⚠"), ("4", "4m")] {
        let found = search(&stand_in, &["--planned", "--min-transfer", min_transfer], true, false).await.unwrap();
        let text = &found.text;
        assert!(text.lines().next().unwrap().ends_with(" · planned times, delays ignored"), "{text}");
        assert!(summary(text).starts_with("fastest 1h50 · "), "{text}");
        assert!(summary(text).contains(" · 6 connections in 6 groups ("), "{text}");
        let lines = connection_lines(text);
        assert!(
            lines.contains(
                &format!(
                    "  08:40 CET → 10:30 CET  1h50  1×  min {buffer}   08:40 CET S1 (31103) → 09:06 CET Bergstadt Hbf ‖{buffer}‖ \
                 09:10 CET ICE 101 → 10:30 CET Zielhafen Hbf"
                )
                .as_str()
            ),
            "--min-transfer {min_transfer}:\n{text}"
        );
        assert!(
            lines.contains(
                &"  08:40 CET → 10:45 CET  2h05  1×  min 24m   08:40 CET S1 (31103) → 09:06 CET Bergstadt Hbf ‖24m‖ 09:30 CET ICE 203 \
                  → 10:45 CET Zielhafen Hbf"
            ),
            "{text}"
        );
        assert!(!text.contains("(+") && !text.contains('✗'), "delays shown:\n{text}");
        assert!(!text.contains("IC 305"), "cancelled departure used:\n{text}");
        assert!(text.contains("Data: Transitous, https://transitous.org/sources/ — planned times."), "{text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_db_only_transitous_and_efa_trains_count() {
    let _serial = SERIAL.lock().await;
    // `--no-db` with credentials, and no credentials at all: the same search.
    let mut reports = Vec::new();
    for (extra, with_credentials) in [(&["--no-db"][..], true), (&[][..], false)] {
        let stand_in = StandIn::start(Efa::Working).await;
        let found = search(&stand_in, extra, with_credentials, false).await.unwrap();
        assert!(stand_in.requests("db").is_empty(), "{:?}", stand_in.requests("db"));
        let warned = found.warnings.iter().any(|w| w.starts_with("no DB_CLIENT_ID/DB_API_KEY"));
        assert_eq!(warned, !with_credentials, "{:?}", found.warnings);
        reports.push(found.text);
    }
    let text = &reports[0];
    assert_eq!(&reports[1], text);
    assert!(summary(text).contains(" · 9 connections in 9 groups ("), "{text}");
    // Transitous doesn't know the cancellation, the delays or the Kreuzhausen stop.
    assert!(
        connection_lines(text).contains(
            &"  08:40 CET → 10:50 CET  2h10  1×  min 14m   08:40 CET S1 (31103) → 09:06 CET Bergstadt Hbf ‖14m‖ 09:20 CET IC 305 \
              → 10:50 CET Zielhafen Hbf"
        ),
        "{text}"
    );
    assert!(text.contains("■ STR 4 … IC 305  (1)"), "EFA's tram missing:\n{text}");
    assert!(!text.contains("Kreuzhausen") && !text.contains("(+") && !text.contains('✗'), "{text}");
    assert!(!text.contains("DB requests") && !text.contains("Deutsche Bahn"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn efa_trouble_costs_its_trams_but_not_the_search() {
    let _serial = SERIAL.lock().await;
    for efa in [Efa::Broken, Efa::Off] {
        let stand_in = StandIn::start(efa).await;
        let found = search(&stand_in, &[], true, false).await.unwrap();
        let text = &found.text;
        assert!(!text.contains("STR 4"), "{text}");
        assert!(summary(text).contains(" · 4 connections in 4 groups, 1 with a change missed"), "{text}");
        assert!(text.contains("■ S1 (31103) … ICE 203  (1)"), "{text}");
        let efa_warnings: Vec<&String> = found.warnings.iter().filter(|w| w.starts_with("EFA ")).collect();
        if efa == Efa::Broken {
            assert!(
                efa_warnings.iter().any(|w| w.starts_with(&format!("EFA {EFA_ORIGIN} → {EFA_HUB}: decoding"))),
                "{:?}",
                found.warnings
            );
            assert!(text.contains(" EFA requests"), "{text}");
        } else {
            assert!(efa_warnings.is_empty(), "{:?}", found.warnings);
            assert_eq!(stand_in.requests("efa"), Vec::<String>::new());
            assert!(!text.contains("EFA requests"), "{text}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn transitous_is_asked_for_the_window_in_slices() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    search(&stand_in, &["--window", "120"], true, false).await.unwrap();
    let plans = stand_in.requests("motis plan");
    // Sent in parallel: in any order.
    let asked = |from: &str, to: &str| -> Vec<String> {
        let prefix = format!("motis plan {from} {to} ");
        let mut asked: Vec<String> = plans.iter().filter_map(|p| p.strip_prefix(&prefix)).map(str::to_owned).collect();
        asked.sort();
        asked
    };
    let slices = |hours: &[&str]| -> Vec<String> { hours.iter().map(|h| format!("{} 3600", iso(berlin(h)))).collect() };
    // The window, an hour per request: each slice keeps its own best departures.
    assert_eq!(asked(AMSEL.id, ZIELHAFEN.id), slices(&["08:00", "09:00"]), "{plans:#?}");
    // Onward from the hub: from 08:00 (two hours before the earliest arrival
    // there, 08:40, but not before the search) to three hours after the latest
    // (09:06 → 12:06).
    assert_eq!(asked(BERGSTADT.id, ZIELHAFEN.id), slices(&["08:00", "09:00", "10:00", "11:00", "12:00"]), "{plans:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_station_fails_the_search() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let mut args = args(&[]);
    args.to = "Atlantis".to_owned();
    let error =
        search::run(&args, &stand_in.endpoints(), keys(true), None, &|_| {}).await.err().expect("no such station");
    assert!(format!("{error:#}").starts_with("no stop named like \"Atlantis\" found"), "{error:#}");
}

#[tokio::test(flavor = "current_thread")]
async fn search_runs_on_a_current_thread_runtime() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let stages = Mutex::new(Vec::new());
    let events = |event: Event<'_>| {
        if let Event::Stage(s) = event {
            stages.lock().unwrap().push(s.to_owned());
        }
    };
    let plot = Some(PlotPage { back: None, details_url: None });
    let outcome = search::run(&args(&[]), &stand_in.endpoints(), keys(true), plot, &events).await.unwrap();
    assert!(summary(&outcome.text).starts_with("fastest 2h01 "), "{}", outcome.text);
    assert!(outcome.plot.is_some());
    let stages = stages.into_inner().unwrap();
    assert_eq!(stages.iter().rev().take(2).collect::<Vec<_>>(), ["Drawing the plot", "Enumerating connections"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn out_of_range_limits_set_in_code_fail_the_search_before_asking() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let mut window = args(&[]);
    window.window = i64::MAX;
    let mut extra = args(&[]);
    extra.extra_hours = f64::INFINITY;
    for (args, option) in [(window, "--window"), (extra, "--extra-hours")] {
        let error =
            search::run(&args, &stand_in.endpoints(), keys(false), None, &|_| {}).await.err().expect("out of range");
        assert!(format!("{error:#}").starts_with(option), "{error:#}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn plot_embeds_every_connection_of_the_report() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let found = search(&stand_in, &[], true, true).await.unwrap();
    let (text, html) = (&found.text, found.html.as_deref().expect("plot asked for"));
    let data = testkit::embedded_json(html, "data");

    assert_eq!(data["back"], Value::Null);
    assert_eq!(data["title"], "Amsel → Zielhafen Hbf");
    let groups: Vec<(String, u64)> = data["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| (g["label"].as_str().unwrap().to_owned(), g["count"].as_u64().unwrap()))
        .collect();
    let reported: Vec<(String, u64)> = text
        .lines()
        .filter_map(|l| l.strip_prefix("■ "))
        .map(|l| {
            let (label, count) = l.rsplit_once("  (").unwrap();
            (label.to_owned(), count.trim_end_matches(')').parse().unwrap())
        })
        .collect();
    assert_eq!(groups, reported);

    let chains = data["chains"].as_array().unwrap();
    let details = data["details"].as_array().unwrap();
    let lines = connection_lines(text);
    assert_eq!((chains.len(), details.len()), (lines.len(), lines.len()));
    for (chain, detail) in chains.iter().zip(details) {
        let line = format!("  {}   {}", chain["header"].as_str().unwrap(), detail["route"].as_str().unwrap());
        assert!(lines.contains(&line.as_str()), "{line} not in\n{text}");
    }

    let names: Vec<&str> = data["names"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    let trips: Vec<&str> = data["trips"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    let pieces = data["pieces"].as_array().unwrap();
    let chain_pieces = |route_start: &str, last: &str| -> Vec<&Value> {
        let (chain, _) = chains
            .iter()
            .zip(details)
            .find(|(_, d)| {
                let route = d["route"].as_str().unwrap();
                route.starts_with(route_start) && route.contains(last)
            })
            .unwrap_or_else(|| panic!("no chain {route_start} … {last}"));
        chain["pieces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| &pieces[usize::try_from(i.as_u64().unwrap()).unwrap()])
            .collect()
    };
    let minute = |hhmm: &str| json!(berlin(hhmm).timestamp().div_euclid(60));
    let point = |p: &Value| (p[0].clone(), names[usize::try_from(p[1].as_u64().unwrap()).unwrap()]);

    // Tram, walk, wait, ICE: the walk runs from the tram stop to the station in 8 minutes.
    let tram = chain_pieces("08:05 CET STR 4", "ICE 101");
    let kinds: Vec<&str> = tram.iter().map(|p| p["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["ride", "walk", "wait", "ride"]);
    let walk: Vec<_> = tram[1]["pts"].as_array().unwrap().iter().map(point).collect();
    assert_eq!(walk, [(minute("08:41"), "Bergstadt Zentrum"), (minute("08:49"), "Bergstadt Hbf")]);
    for ride in tram.iter().filter(|p| p["kind"] == "ride") {
        let label = trips[usize::try_from(ride["trip"].as_u64().unwrap()).unwrap()];
        assert!(ride["info"].as_str().unwrap().starts_with(label), "{ride} vs {label}");
    }

    // The missed change: a red wait drawn forward from the departure to the late arrival.
    let missed = chain_pieces("08:48 CET (+8) S1 (31103)", "ICE 101");
    let wait = missed.iter().find(|p| p["kind"] == "wait").unwrap();
    assert_eq!(wait["warn"], true);
    assert_eq!(wait["info"], "missed 2m✗ at Bergstadt Hbf");
    let pts: Vec<_> = wait["pts"].as_array().unwrap().iter().map(point).collect();
    assert_eq!(pts, [(minute("09:10"), "Bergstadt Hbf"), (minute("09:12"), "Bergstadt Hbf")]);

    // Live times in the drawing: ICE 203 reaches Zielhafen at 10:49, not 10:45.
    let late = chain_pieces("08:10 CET S1 (31101)", "ICE 203");
    let ice = late.last().unwrap()["pts"].as_array().unwrap().iter().map(point).collect::<Vec<_>>();
    assert_eq!(ice.first(), Some(&(minute("09:30"), "Bergstadt Hbf")));
    assert_eq!(ice.last(), Some(&(minute("10:49"), "Zielhafen Hbf")));
}

// ---------------------------------------------------------------------------
// The web front end

/// The finished job's status. Searches here take about a second; giving up
/// after 10 s fails a job that never finishes well within the mutation tests'
/// 20 s timeout.
async fn wait_until_done(http: &reqwest::Client, status_url: &str) -> Value {
    let poll = async {
        loop {
            let status: Value = http.get(status_url).send().await.unwrap().json().await.unwrap();
            if status["done"] == true {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), poll)
        .await
        .unwrap_or_else(|_| panic!("search behind {status_url} didn't finish"))
}

/// A search started from form values: its link (the page showing it), where
/// its status is polled, and the raw query.
struct Started {
    link: String,
    status: String,
    query: String,
}

/// Starts a search from form values by opening its link, which shows its progress.
async fn start_search(http: &reqwest::Client, site: &str, form: &[(&str, &str)]) -> Started {
    let mut url = reqwest::Url::parse(&format!("{site}/search")).unwrap();
    url.query_pairs_mut().extend_pairs(form);
    let query = url.query().unwrap().to_owned();
    let response = http.get(url.clone()).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{form:?}");
    let data = testkit::embedded_json(&response.text().await.unwrap(), "data");
    let status = data["status"].as_str().unwrap_or_else(|| panic!("no progress page: {data}"));
    Started { link: url.to_string(), status: format!("{site}{status}"), query }
}

#[tokio::test(flavor = "multi_thread")]
async fn web_front_end_runs_a_search_to_its_plot() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let router = crate::serve::test_router(1, stand_in.endpoints(), fake_keys).unwrap();
    let site = testkit::serve(router).await;
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();

    let form = http.get(format!("{site}/")).send().await.unwrap().text().await.unwrap();
    let spec = testkit::embedded_json(&form, "spec");
    assert_eq!(spec["error"], Value::Null);
    // The user's zone, not the server's or the stations': 08:00 in Berlin is 02:00 in New York.
    let Started { link, status: status_url, query } = start_search(
        &http,
        &site,
        &[
            ("from", "Amsel"),
            ("to", "Zielhafen Hbf"),
            ("at", &format!("{}T02:00", day())),
            ("time_zone", "America/New_York"),
            ("window", "60"),
        ],
    )
    .await;

    let status = wait_until_done(&http, &status_url).await;
    assert_eq!(status["error"], Value::Null, "{status}");
    let back = format!("/?{query}");
    assert_eq!(status["back"], back.as_str());
    let log: Vec<&str> = status["log"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
    for stage in ["Finding stations", "Enumerating connections", "Drawing the plot", "Done"] {
        assert!(log.iter().any(|l| l.ends_with(stage)), "{stage} missing in {log:#?}");
    }
    // The server's credentials were used: DB's live data is in the plot.
    assert!(!log.iter().any(|l| l.contains("warning")), "{log:#?}");

    // The same link, now that the search is done: its plot.
    let plot = http.get(&link).send().await.unwrap();
    assert_eq!(plot.status(), StatusCode::OK);
    let data = testkit::embedded_json(&plot.text().await.unwrap(), "data");
    assert_eq!(data["back"], back.as_str());
    assert_eq!(data["tz"], "America/New_York", "the plot's time axis");
    // The form lists the best connections; the network drawn has all six.
    assert_eq!(data["chains"].as_array().unwrap().len(), 4);
    let subtitle = data["subtitle"].as_str().unwrap();
    assert!(subtitle.starts_with("departing Tue 3 Nov 2026, 02:00 EST–03:00 EST · "), "{data}");
    assert!(subtitle.contains(" · 6 connections, the 4 best in "), "{subtitle}");
    // Details aren't in the page: it asks for them by connection. ICE 203 at 10:49 in Berlin.
    assert_eq!(data["details"], Value::Null);
    let details_url = format!("{site}{}?ids=0,1,2,3", data["details_url"].as_str().unwrap());
    let details: Value = http.get(details_url).send().await.unwrap().json().await.unwrap();
    assert!(details.as_array().unwrap().iter().any(|d| d["route"].as_str().unwrap().contains("04:49 EST (+4)")));
    // The back link brings the form up with the same values.
    let again = http.get(format!("{site}{back}")).send().await.unwrap().text().await.unwrap();
    let values = &testkit::embedded_json(&again, "spec")["values"];
    assert_eq!((values["from"].as_str(), values["to"].as_str()), (Some("Amsel"), Some("Zielhafen Hbf")));
    assert_eq!(values["time_zone"], "America/New_York");
}

#[tokio::test(flavor = "multi_thread")]
async fn web_front_end_reports_bad_input() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let router = crate::serve::test_router(1, stand_in.endpoints(), fake_keys).unwrap();
    let site = testkit::serve(router).await;
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();

    // Invalid options: the form again, with clap's error and the values entered.
    let mut url = reqwest::Url::parse(&format!("{site}/search")).unwrap();
    url.query_pairs_mut().extend_pairs([("from", "Amsel"), ("to", "Zielhafen Hbf"), ("window", "soon")]);
    let response = http.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let spec = testkit::embedded_json(&response.text().await.unwrap(), "spec");
    let error = spec["error"].as_str().unwrap_or_else(|| panic!("no error in {spec}"));
    assert!(error.contains("'soon'") && error.contains("--window"), "{error}");
    assert_eq!(spec["values"]["window"], "soon");

    // An unknown station fails the search; the status says why and there is no plot.
    let start = start_at();
    let started = start_search(
        &http,
        &site,
        &[("from", "Atlantis"), ("to", "Zielhafen Hbf"), ("at", &start), ("time_zone", "Europe/Berlin")],
    )
    .await;
    let status = wait_until_done(&http, &started.status).await;
    let error = status["error"].as_str().unwrap_or_else(|| panic!("no error in {status}"));
    assert!(error.starts_with("no stop named like \"Atlantis\""), "{error}");
    assert!(status["log"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().ends_with("Failed")), "{status}");
    let again = testkit::embedded_json(&http.get(&started.link).send().await.unwrap().text().await.unwrap(), "data");
    assert!(again["status"].is_string(), "the failed search runs again: {again}");
    wait_until_done(&http, &format!("{site}{}", again["status"].as_str().unwrap())).await;

    // Nothing runs: no plot, and the status carries the report saying so.
    let afternoon = at("14:00");
    let started = start_search(
        &http,
        &site,
        &[("from", "Amsel"), ("to", "Zielhafen Hbf"), ("at", &afternoon), ("time_zone", "Europe/Berlin")],
    )
    .await;
    let status = wait_until_done(&http, &started.status).await;
    let error = status["error"].as_str().unwrap_or_else(|| panic!("no error in {status}"));
    assert!(error.ends_with("\nNo connection found.\n"), "{error}");
}

// ---------------------------------------------------------------------------
// Report variants

/// The lines of the connection whose header is `header`, up to the next one.
fn block<'a>(text: &'a str, header: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| *l == header).unwrap_or_else(|| panic!("no {header:?} in\n{text}"));
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.is_empty() || l.starts_with('■') || is_connection(l))
        .map_or(lines.len(), |n| start + 1 + n);
    lines[start..end].to_vec()
}

#[tokio::test(flavor = "multi_thread")]
async fn detail_shows_platforms_sources_and_fallbacks() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::with_regional().await;
    let found = search(&stand_in, &["--detail"], true, false).await.unwrap();
    let text = &found.text;
    assert!(summary(text).contains(" · 9 connections in 9 groups, 1 with a change missed"), "{text}");
    assert!(!text.contains('‖'), "one-line routes in the detail report:\n{text}");

    // Fallbacks for a missed ICE 101: later trains from the change station,
    // earliest arrival first, the lateness against the plan.
    assert_eq!(
        block(text, "  08:10 CET → 10:30 CET  2h20  1×  min 30m"),
        [
            "  08:10 CET → 10:30 CET  2h20  1×  min 30m",
            "    08:10 CET Amsel [1] → 08:40 CET Bergstadt Hbf [1]   S1 (31101) → Bergstadt Hbf  (DB)",
            "      ↳ 30m buffer · if missed:",
            "          09:30 CET ICE 203 → 10:49 CET (+4) Zielhafen Hbf · arrive 10:49 CET (+19m)",
            "          09:15 CET RE 4711 → 11:10 CET Zielhafen Hbf · arrive 11:10 CET (+40m)",
            "    09:10 CET Bergstadt Hbf [1] → 10:30 CET Zielhafen Hbf [1]   ICE 101 → Zielhafen Hbf  (DB)",
        ],
        "{text}"
    );
    // RE 4711's arrival comes from Transitous (no platform); a fallback arriving
    // earlier than the plan is no delay; a short change is flagged.
    assert_eq!(
        block(text, "  08:48 CET → 11:10 CET  2h22  1×  min 3m⚠"),
        [
            "  08:48 CET → 11:10 CET  2h22  1×  min 3m⚠",
            "    08:48 CET (+8) Amsel [1] → 09:12 CET (+6) Bergstadt Hbf [1]   S1 (31103) → Bergstadt Hbf  (DB)",
            "      ↳ 3m⚠ buffer · if missed:",
            "          09:30 CET ICE 203 → 10:49 CET (+4) Zielhafen Hbf · arrive 10:49 CET (+0m)",
            "    09:15 CET Bergstadt Hbf [1] → 11:10 CET Zielhafen Hbf   RE 4711 → Zielhafen Hbf  (DB/T)",
        ],
        "{text}"
    );
    // Nothing leaves Bergstadt after ICE 203 (IC 305 is earlier, and cancelled).
    assert_eq!(
        block(text, "  08:10 CET → 10:49 CET  2h39  1×  min 50m")[2..4],
        ["      ↳ 50m buffer · if missed:", "          nothing within 2h00"],
        "{text}"
    );
    // EFA's tram is Transitous-side ("T"); the walk gets its own line before the next train.
    let tram = block(text, "  08:05 CET → 10:30 CET  2h25  1×  min 21m");
    assert_eq!(
        tram[1], "    08:05 CET Amsel [1] → 08:41 CET Bergstadt Zentrum [1]   STR 4 → Bergstadt Zentrum  (T)",
        "{text}"
    );
    assert_eq!(tram[2], "      ↳ 21m buffer · if missed:", "{text}");
    assert_eq!(
        tram[tram.len() - 2..],
        [
            "      walk 8m to Bergstadt Hbf",
            "    09:10 CET Bergstadt Hbf [1] → 10:30 CET Zielhafen Hbf [1]   ICE 101 → Zielhafen Hbf  (DB)",
        ],
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn detail_fallbacks_end_back_on_plan() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::with_regional().await;
    let found = search(&stand_in, &["--detail", "--keep-detours"], true, false).await.unwrap();
    let text = &found.text;
    // Missing ICE 101 on the detour via Kreuzhausen: ICE 203 from Bergstadt is the
    // plan's last train, so the trip continues as planned; RE 4711 is 21 min later.
    assert_eq!(
        block(text, "  08:10 CET → 10:49 CET  2h39  2×  min 24m"),
        [
            "  08:10 CET → 10:49 CET  2h39  2×  min 24m",
            "    08:10 CET Amsel [1] → 08:40 CET Bergstadt Hbf [1]   S1 (31101) → Bergstadt Hbf  (DB)",
            "      ↳ 30m buffer · if missed:",
            "          09:30 CET ICE 203 (back on plan) · arrive 10:49 CET (+0m)",
            "          09:15 CET RE 4711 → 11:10 CET Zielhafen Hbf · arrive 11:10 CET (+21m)",
            "    09:10 CET Bergstadt Hbf [1] → 09:40 CET Kreuzhausen [1]   ICE 101 → Zielhafen Hbf  (DB)",
            "      ↳ 24m buffer · if missed:",
            "          nothing within 2h00",
            "    10:04 CET (+4) Kreuzhausen [1] → 10:49 CET (+4) Zielhafen Hbf [1]   ICE 203 → Zielhafen Hbf  (DB)",
        ],
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_running_is_reported_without_a_plot() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let afternoon = at("14:00");
    let found = search(&stand_in, &["--at", &afternoon], true, true).await.unwrap();
    let text = &found.text;
    assert!(found.html.is_none(), "plot of nothing");
    // The window is printed in --time-zone.
    assert!(text.starts_with("Amsel → Zielhafen Hbf, departing Tue 3 Nov 2026, 14:00 CET–15:00 CET\n"), "{text}");
    assert!(text.ends_with("\n\nNo connection found.\n"), "{text}");
    assert!(!text.contains("fastest") && !text.contains("Data:"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn effort_cap_and_group_size_are_reported() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let suffix = |text: &str| summary(text).rsplit_once(" states explored").unwrap().1.to_owned();

    // Capped before any connection is complete.
    let found = search(&stand_in, &["--max-nodes", "1"], true, false).await.unwrap();
    assert!(summary(&found.text).contains(" · 0 connections in 0 groups ("), "{}", found.text);
    assert_eq!(suffix(&found.text), ", incomplete even without changes; raise --max-nodes)");
    assert!(connection_lines(&found.text).is_empty(), "{}", found.text);

    // Capped after a few rounds: complete up to some changes, and every
    // connection (all have one change) is listed.
    let found = search(&stand_in, &["--max-nodes", "60"], true, false).await.unwrap();
    let rest = suffix(&found.text);
    let changes: usize = rest
        .strip_prefix(", complete up to ")
        .and_then(|r| r.strip_suffix(" changes; raise --max-nodes for more)"))
        .unwrap_or_else(|| panic!("{rest}"))
        .parse()
        .unwrap();
    assert!((1..15).contains(&changes), "{rest}");
    assert_eq!(connection_lines(&found.text).len(), 6, "{}", found.text);

    // Uncapped: no remark. Limited to one per group: the rest counted.
    let found = search(&stand_in, &["--keep-detours", "--per-group", "1"], true, false).await.unwrap();
    let text = &found.text;
    assert_eq!(suffix(text), ")");
    let lines: Vec<&str> = text.lines().collect();
    let group = lines.iter().position(|l| *l == "■ S1 (31101) … ICE 203  (2)").unwrap_or_else(|| panic!("{text}"));
    assert!(lines[group + 1].ends_with("‖50m‖ 09:30 CET ICE 203 → 10:49 CET (+4) Zielhafen Hbf"), "{text}");
    assert_eq!(lines[group + 2], "    … 1 more", "{text}");
    assert_eq!(connection_lines(text).len(), 6, "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn trafikverket_only_looks_whether_a_search_passes_through_sweden() {
    let _serial = SERIAL.lock().await;
    let stand_in = StandIn::start(Efa::Working).await;
    let sweden = testkit::TvWorld::new(vec![("G", "Göteborg C", "SE", 57.709, 11.974)], Vec::new());
    let endpoints = Endpoints { trafikverket_api: testkit::serve_trafikverket(&sweden).await, ..stand_in.endpoints() };
    let with_key = || Keys { db: Some(credentials()), trafikverket: Some("key".to_owned()) };
    let outcome = search::run(&args(&[]), &endpoints, with_key(), None, &|_| {}).await.unwrap();
    // The station list (cached from now on) says this German search doesn't: no board asked, no credit.
    assert_eq!(sweden.log(), ["stations"]);
    assert!(outcome.text.contains(", 1 Trafikverket requests"), "{}", outcome.text);
    assert!(!outcome.text.contains("Trafikverket open data"), "{}", outcome.text);

    let outcome = search::run(&args(&["--no-trafikverket"]), &endpoints, with_key(), None, &|_| {}).await.unwrap();
    assert!(!outcome.text.contains("Trafikverket"), "{}", outcome.text);
    assert_eq!(sweden.log(), ["stations"]);
}
