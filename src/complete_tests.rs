use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use serde_json::{Value, json};

use super::*;
use crate::testkit::{Builder, at, ride};
use crate::timetable::add_stops;

/// FLX 1236 as DB boards know it: Frankfurt Süd and Hamburg Hbf only.
fn flx() -> Timetable {
    let mut b = Builder::new();
    b.trip("FLX 1236", &[("Frankfurt Süd", "06:55", "06:58"), ("Hamburg Hbf", "13:21", "")]);
    b.trip("ICE 76", &[("Frankfurt Süd", "", "06:54"), ("Hamburg Hbf", "10:31", "")]);
    let mut tt = b.build("Frankfurt Süd", "Hamburg Hbf");
    tt.stations[0].motis_id = Some("de-DELFI_de:06412:912_G".to_owned());
    tt
}

fn chain(steps: Vec<Step>) -> Chain {
    Chain { steps }
}

#[test]
fn wanted_are_db_only_trains_of_the_connections_widest_gap_first() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "08:30", "")]); // 30-minute gap
    b.trip("ICE 2", &[("A", "", "08:00"), ("B", "09:00", "")]); // 60
    b.trip("S 3", &[("A", "", "08:00"), ("B", "08:19", "")]); // too short a gap
    b.trip("IC 4", &[("A", "", "08:00"), ("B", "10:00", "")]); // not ridden
    let merged = b.trip("EC 5", &[("A", "", "08:00"), ("B", "10:00", "")]);
    let mut tt = b.build("A", "B");
    tt.trips[merged].transitous = true; // its stops are known already
    let chains: Vec<Chain> =
        ["RE 1", "ICE 2", "S 3", "EC 5", "RE 1"].iter().map(|&train| chain(vec![ride(&tt, train, "A", "B")])).collect();
    assert_eq!(wanted(&tt, &chains), [1, 0]);
}

#[test]
fn wanted_are_capped() {
    let mut b = Builder::new();
    let trips: Vec<usize> =
        (0..MAX_TRAINS + 5).map(|i| b.trip(&format!("RE {i}"), &[("A", "", "08:00"), ("B", "09:00", "")])).collect();
    let tt = b.build("A", "B");
    let chains: Vec<Chain> = trips.iter().map(|&trip| chain(vec![Step::Ride { trip, board: 0, alight: 1 }])).collect();
    assert_eq!(wanted(&tt, &chains).len(), MAX_TRAINS);
}

#[test]
fn numbers_are_whole_runs_of_digits() {
    assert!(mentions("ICE 614", 614));
    assert!(mentions("20261026_05:15_eu-flixbus_FLX10-1236-0515102026-TS#HH-00", 1236));
    assert!(mentions("011174", 11174));
    assert!(!mentions("ICE 6140", 614));
    assert!(!mentions("FLX10-1236", 123));
}

#[derive(Default)]
struct Fake {
    stoptimes: AtomicUsize,
    trips: Mutex<Vec<String>>,
    asked: Mutex<Vec<HashMap<String, String>>>,
}

/// A Transitous stand-in answering departures at Frankfurt Süd and FLX 1236's trip.
async fn transitous(fake: &Arc<Fake>, scope: &str) -> Motis {
    async fn stoptimes(State(fake): State<Arc<Fake>>, Query(q): Query<HashMap<String, String>>) -> axum::Json<Value> {
        fake.stoptimes.fetch_add(1, Ordering::SeqCst);
        fake.asked.lock().unwrap().push(q);
        let stop = |name: &str, time: &str, trip: &str, short: &str| {
            json!({
                "place": { "name": "Frankfurt (Main) Süd", "scheduledDeparture": time, "departure": time },
                "mode": "LONG_DISTANCE",
                "tripId": trip,
                "displayName": name,
                "tripShortName": short,
            })
        };
        axum::Json(json!({ "stopTimes": [
            stop("ICE 76", "2026-10-24T06:54:00Z", "20261024_de-DELFI_3448", "ICE 76"),
            // A FlixTrain at the same minute, but another number.
            stop("FlixTrain FLX10", "2026-10-24T06:58:00Z", "20261024_eu-flixbus_FLX10-1999-0515", ""),
            stop("FlixTrain FLX10", "2026-10-24T06:59:00Z", "20261024_eu-flixbus_FLX10-1236-0515", ""),
        ]}))
    }
    async fn trip(State(fake): State<Arc<Fake>>, Query(q): Query<HashMap<String, String>>) -> axum::Json<Value> {
        fake.trips.lock().unwrap().push(q["tripId"].clone());
        let place = |name: &str, lat: f64, arrival: Option<&str>, departure: Option<&str>| {
            json!({ "name": name, "lat": lat, "lon": 9.0, "scheduledArrival": arrival, "arrival": arrival,
                    "scheduledDeparture": departure, "departure": departure })
        };
        axum::Json(json!({ "legs": [{
            "mode": "LONG_DISTANCE",
            "tripId": q["tripId"],
            "startTime": "2026-10-24T05:15:00Z",
            "endTime": "2026-10-24T13:21:00Z",
            "from": place("Stuttgart (FlixTrain)", 48.0, None, Some("2026-10-24T05:15:00Z")),
            "intermediateStops": [
                place("Frankfurt Süd", 50.1, Some("2026-10-24T06:55:00Z"), Some("2026-10-24T06:58:00Z")),
                place("Fulda (FlixTrain)", 50.5, Some("2026-10-24T07:54:00Z"), Some("2026-10-24T07:56:00Z")),
                place("Berlin Hbf (FlixTrain)", 52.5, Some("2026-10-24T11:01:00Z"), Some("2026-10-24T11:18:00Z")),
            ],
            "to": place("Hamburg Hbf", 53.5, Some("2026-10-24T13:21:00Z"), None),
        }]}))
    }
    let router = Router::new()
        .route(&format!("/{scope}/api/v5/stoptimes"), get(stoptimes))
        .route(&format!("/{scope}/api/v5/trip"), get(trip))
        .with_state(fake.clone());
    // Answers are cached by URL, and ports get reused: a scope per test.
    Motis::new(&format!("{}/{scope}", crate::testkit::serve(router).await), "umstieg-test").unwrap()
}

#[tokio::test]
async fn lookup_finds_the_train_by_time_and_number_in_its_trip_id() {
    let fake = Arc::new(Fake::default());
    let motis = transitous(&fake, "complete-found").await;
    let tt = flx();
    let leg = lookup(&motis, &tt, 0).await.unwrap().unwrap();
    assert_eq!(leg.trip_id.as_deref(), Some("20261024_eu-flixbus_FLX10-1236-0515"));
    let asked = fake.asked.lock().unwrap();
    assert_eq!(asked[0]["stopId"], "de-DELFI_de:06412:912_G");
    assert_eq!(asked[0]["time"], "2026-10-24T06:56:00Z", "from two minutes before the departure");
}

#[tokio::test]
async fn lookup_needs_a_station_transitous_knows_and_the_train_there() {
    let fake = Arc::new(Fake::default());
    let motis = transitous(&fake, "complete-missing").await;
    // No Transitous id at the only station the train departs from.
    let mut tt = flx();
    tt.stations[0].motis_id = None;
    assert!(lookup(&motis, &tt, 0).await.unwrap().is_none());
    assert_eq!(fake.stoptimes.load(Ordering::SeqCst), 0, "nothing asked");
    // Known there, but Transitous has no such train at that time.
    let mut b = Builder::new();
    b.trip("FLX 1236", &[("Frankfurt Süd", "", "07:30"), ("Hamburg Hbf", "13:21", "")]);
    let mut tt = b.build("Frankfurt Süd", "Hamburg Hbf");
    tt.stations[0].motis_id = Some("x".to_owned());
    assert!(lookup(&motis, &tt, 0).await.unwrap().is_none());
    assert!(fake.trips.lock().unwrap().is_empty(), "no trip asked for");
}

#[tokio::test]
async fn found_stops_fill_the_gap_and_connections_keep_their_rides() {
    let fake = Arc::new(Fake::default());
    let motis = transitous(&fake, "complete-fill").await;
    let mut tt = flx();
    let mut chains = vec![chain(vec![ride(&tt, "FLX 1236", "Frankfurt Süd", "Hamburg Hbf")])];
    let leg = lookup(&motis, &tt, 0).await.unwrap().unwrap();
    let moved = add_stops(&mut tt, 0, &leg).unwrap();
    remap(&mut chains, 0, &moved);
    let names: Vec<&str> = tt.trips[0].calls.iter().map(|c| tt.stations[c.station].name.as_str()).collect();
    // Only between the known calls; Stuttgart is before Frankfurt Süd.
    assert_eq!(names, ["Frankfurt Süd", "Fulda (FlixTrain)", "Berlin Hbf (FlixTrain)", "Hamburg Hbf"]);
    assert_eq!(moved, [0, 3]);
    let Step::Ride { board, alight, .. } = chains[0].steps[0] else { panic!("a ride") };
    assert_eq!((tt.trips[0].calls[board].station, tt.trips[0].calls[alight].station), (0, 1));
    // Other trains' rides are untouched.
    let mut other = vec![chain(vec![Step::Ride { trip: 1, board: 0, alight: 1 }])];
    remap(&mut other, 0, &moved);
    assert!(matches!(other[0].steps[0], Step::Ride { trip: 1, board: 0, alight: 1 }));
}

#[test]
fn a_copy_disagreeing_where_both_stop_adds_nothing() {
    let leg = |frankfurt: &str| -> Leg {
        serde_json::from_value(json!({
            "mode": "LONG_DISTANCE",
            "tripId": "t",
            "startTime": frankfurt,
            "endTime": "2026-10-24T13:21:00Z",
            "from": { "name": "Frankfurt Süd", "scheduledDeparture": frankfurt },
            "intermediateStops": [{ "name": "Fulda", "scheduledArrival": "2026-10-24T07:54:00Z",
                                    "scheduledDeparture": "2026-10-24T07:56:00Z" }],
            "to": { "name": "Hamburg Hbf", "scheduledArrival": "2026-10-24T13:21:00Z" },
        }))
        .unwrap()
    };
    // Twelve minutes off at Frankfurt Süd: another train, or one of the feeds is wrong.
    let mut tt = flx();
    assert!(add_stops(&mut tt, 0, &leg("2026-10-24T07:10:00Z")).is_none());
    assert_eq!(tt.trips[0].calls.len(), 2);
    // Within two minutes: the same train.
    assert_eq!(add_stops(&mut tt, 0, &leg("2026-10-24T07:00:00Z")), Some(vec![0, 2]));
    // Nothing new the second time.
    assert!(add_stops(&mut tt, 0, &leg("2026-10-24T07:00:00Z")).is_none());
}

#[test]
fn added_stops_take_on_the_trains_delay() {
    // RE7 (11172): DB has it 5 minutes late at Schleswig and Flensburg;
    // Transitous's copy, planned times only, adds Jübek in between.
    let mut b = Builder::new();
    b.trip("RE7 (11172)", &[("Schleswig", "16:16+4", "16:17+5"), ("Flensburg", "16:41+5", "")]);
    let mut tt = b.build("Schleswig", "Flensburg");
    let leg: Leg = serde_json::from_value(json!({
        "mode": "REGIONAL_RAIL",
        "tripId": "t",
        "startTime": "2026-10-24T16:17:00Z",
        "endTime": "2026-10-24T16:41:00Z",
        "from": { "name": "Schleswig", "scheduledDeparture": "2026-10-24T16:17:00Z" },
        "intermediateStops": [{ "name": "Jübek", "scheduledArrival": "2026-10-24T16:23:00Z",
                                "scheduledDeparture": "2026-10-24T16:24:00Z" }],
        "to": { "name": "Flensburg", "scheduledArrival": "2026-10-24T16:41:00Z" },
    }))
    .unwrap();
    add_stops(&mut tt, 0, &leg).unwrap();
    let jubek = &tt.trips[0].calls[1];
    assert_eq!(tt.stations[jubek.station].name, "Jübek");
    let actual = |t: Option<crate::timetable::Time>| t.map(|t| t.actual);
    assert_eq!((actual(jubek.arrival), actual(jubek.departure)), (Some(at("16:28")), Some(at("16:29"))));
}
