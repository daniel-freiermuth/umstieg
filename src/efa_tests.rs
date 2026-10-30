#![expect(clippy::arithmetic_side_effects, reason = "fixture times")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde_json::json;

use super::*;
use crate::testkit::{self, at};
use crate::timetable::ProductClass;

const TRIPS: &str = include_str!("../testdata/efa/trips_bad_duerkheim.json");

fn itineraries(text: &str) -> Vec<Itinerary> {
    let response: TripResponse = serde_json::from_str(text).unwrap();
    response.journeys.iter().filter_map(itinerary).collect()
}

fn names(places: &[Place]) -> Vec<&str> {
    places.iter().map(|p| p.name.as_str()).collect()
}

#[test]
fn dhids_from_delfi_stop_ids() {
    let cases = [
        ("de-DELFI_de:07332:116_G", Some("de:07332:116")),
        ("de-DELFI_de:08222:2417", Some("de:08222:2417")),
        ("de-DELFI_de:08222:2417:4:2", Some("de:08222:2417:4:2")),
        ("de-DELFI_000008601770", None),
        ("de-VBB_de:11000:900003201", None),
        ("dk-rejseplanen_000008600626", None),
        ("de:07332:116", None),
        ("", None),
    ];
    for (id, expected) in cases {
        assert_eq!(dhid(id).as_deref(), expected, "{id:?}");
    }
}

#[test]
fn product_classes_map_to_motis_modes() {
    let cases = [
        (0, Some(Mode::RegionalRail)),
        (13, Some(Mode::RegionalRail)),
        (14, Some(Mode::HighspeedRail)),
        (15, Some(Mode::HighspeedRail)),
        (16, Some(Mode::HighspeedRail)),
        (1, Some(Mode::Suburban)),
        (2, Some(Mode::Subway)),
        (3, Some(Mode::Tram)),
        (4, Some(Mode::Tram)),
        (9, Some(Mode::Ferry)),
        (5, Some(Mode::Bus)),
        (6, Some(Mode::Bus)),
        (7, Some(Mode::Bus)),
        (10, Some(Mode::Bus)),
        (99, None),
        (100, None),
        (105, None),
        (106, None),
        (107, None),
    ];
    for (class, expected) in cases {
        assert_eq!(mode(class), expected, "class {class}");
    }
}

#[test]
fn journeys_become_itineraries() {
    let all = itineraries(TRIPS);
    assert_eq!(all.len(), 4);

    // RNV 4 then RNV 6, changing at Ludwigshafen Berliner Platz.
    let [rnv4, rnv6] = &all[0].legs[..] else { panic!("two legs: {:?}", all[0].legs.len()) };
    assert_eq!((rnv4.mode.as_str(), rnv4.label(), rnv4.class()), ("TRAM", "RNV 4".to_owned(), None));
    assert_eq!((rnv4.start_time, rnv4.end_time), (at("02:13"), at("02:58")));
    assert_eq!(rnv4.trip_id.as_deref(), Some("efa:vrn:08004:s:H:j26:515:20261024"));
    assert_eq!(rnv4.headsign.as_deref(), Some("Waldhof, Waldfriedhof"));
    assert_eq!(rnv4.from.name, "Bad Dürkheim, Bahnhof");
    assert_eq!(rnv4.from.stop_id.as_deref(), Some("de:07332:116:2:Tram"));
    assert_eq!((rnv4.from.lat, rnv4.from.lon), (Some(49.461_452), Some(8.170_438)), "EFA coordinates are [lat, lon]");
    assert_eq!(rnv4.from.track.as_deref(), Some("G1"));
    assert_eq!(names(&rnv4.intermediate_stops), ["Bad Dürkheim, Ost", "Friedelsheim, Bahnhof"], "ends excluded");
    assert_eq!(rnv4.intermediate_stops[0].departure, Some(at("02:16")));
    assert_eq!(rnv6.trip_id.as_deref(), Some("efa:vrn:08006:s:R:j26:125:20261024"));
    assert_eq!(names(&rnv6.intermediate_stops), ["Mannheim, Universität"]);

    // The same tram run in another journey gets the same id, so the timetable merges them.
    let [same_run, walk] = &all[1].legs[..] else { panic!("tram and walk") };
    assert_eq!(same_run.trip_id, rnv4.trip_id);
    assert!(!walk.is_transit());
    assert_eq!(walk.mode, Mode::Walk);
    assert_eq!((walk.from.name.as_str(), walk.to.name.as_str()), ("Mannheim, Universität", "Mannheim, Hauptbahnhof"));
    assert_eq!((walk.start_time, walk.end_time), (at("03:03"), at("03:13")));

    // A later run of the same line is another trip.
    let s6 = &all[2].legs[2];
    assert_ne!(all[2].legs[0].trip_id, rnv4.trip_id);
    assert_eq!((s6.mode.as_str(), s6.class(), s6.train_number()), ("SUBURBAN", Some(ProductClass::SBahn), Some(38617)));
    assert_eq!(s6.label(), "S6 (38617)");
    assert_eq!((all[2].legs[1].start_time, all[2].legs[1].end_time), (at("04:40"), at("04:47")));

    // Live times where EFA estimates them, planned ones kept as scheduled.
    let s3 = &all[3].legs[0];
    assert_eq!((s3.start_time, s3.end_time), (at("21:09"), at("21:26")));
    assert_eq!((s3.from.departure, s3.from.scheduled_departure), (Some(at("21:09")), Some(at("21:04"))));
    assert_eq!((s3.to.arrival, s3.to.scheduled_arrival), (Some(at("21:26")), Some(at("21:22"))));
    assert_eq!(s3.trip_id.as_deref(), Some("efa:ddb:920S3:D:H:j26:245:20261024"));
}

/// A journey of `legs`, each (class, departure, arrival, extra leg fields).
fn journey(legs: &[(i64, Option<&str>, Option<&str>, serde_json::Value)]) -> String {
    let legs: Vec<serde_json::Value> = legs
        .iter()
        .enumerate()
        .map(|(i, (class, departure, arrival, extra))| {
            let mut leg = json!({
                "duration": 600,
                "origin": { "name": format!("S{i}"), "departureTimePlanned": departure },
                "destination": { "name": format!("S{}", i + 1), "arrivalTimePlanned": arrival },
                "transportation": { "id": "line", "number": "RE1", "product": { "class": class }, "properties": { "tripCode": 7 } },
            });
            for (key, value) in extra.as_object().unwrap() {
                leg[key] = value.clone();
            }
            leg
        })
        .collect();
    json!({ "journeys": [{ "legs": legs }] }).to_string()
}

#[test]
fn walks_without_times_follow_the_previous_leg() {
    let text = journey(&[
        (0, Some("2026-10-24T08:00:00Z"), Some("2026-10-24T08:30:00Z"), json!({})),
        (100, None, None, json!({ "duration": 420 })),
        (0, Some("2026-10-24T08:45:00Z"), Some("2026-10-24T09:00:00Z"), json!({})),
        (99, None, None, json!({ "duration": 120 })),
    ]);
    let legs = &itineraries(&text)[0].legs;
    assert_eq!(legs.len(), 4);
    assert_eq!((legs[1].start_time, legs[1].end_time), (at("08:30"), at("08:37")));
    assert_eq!((legs[3].start_time, legs[3].end_time), (at("09:00"), at("09:02")));
    assert!(legs[1].trip_id.is_none() && legs[3].trip_id.is_none());
}

#[test]
fn journeys_without_usable_transit_are_dropped() {
    let cases = [
        ("walks only", journey(&[(99, Some("2026-10-24T08:00:00Z"), None, json!({}))])),
        ("no departure time", journey(&[(0, None, Some("2026-10-24T08:30:00Z"), json!({}))])),
        ("no arrival time", journey(&[(0, Some("2026-10-24T08:00:00Z"), None, json!({}))])),
        ("no legs", json!({ "journeys": [{}] }).to_string()),
    ];
    for (what, text) in cases {
        assert!(itineraries(&text).is_empty(), "{what}");
    }
}

#[test]
fn walks_too_long_for_any_date_drop_the_journey() {
    for duration in [i64::MAX, 1_000_000_000_000_000] {
        let text = journey(&[
            (0, Some("2026-10-24T08:00:00Z"), Some("2026-10-24T08:30:00Z"), json!({})),
            (100, None, None, json!({ "duration": duration })),
        ]);
        assert!(itineraries(&text).is_empty(), "{duration} s");
    }
}

#[test]
fn runs_are_told_apart_by_scheduled_day_in_germany() {
    let at_time = |planned: &str, estimated: &str| {
        let text = journey(&[(
            0,
            Some(planned),
            Some("2026-10-25T03:00:00Z"),
            json!({ "origin": { "name": "A", "departureTimePlanned": planned, "departureTimeEstimated": estimated } }),
        )]);
        itineraries(&text)[0].legs[0].trip_id.clone().unwrap()
    };
    // 23:55 in Germany, ten minutes late: still the run of the 24th.
    assert_eq!(at_time("2026-10-24T21:55:00Z", "2026-10-24T22:05:00Z"), "efa:line:7:20261024");
    // 00:05 in Germany is the next day although UTC still says the 24th.
    assert_eq!(at_time("2026-10-24T22:05:00Z", "2026-10-24T22:05:00Z"), "efa:line:7:20261025");
}

#[test]
fn trains_are_labelled_with_the_category_in_their_name() {
    // (product class, name, number) → (label, DB class).
    let cases = [
        // VRN files ICEs under the generic class "Zug".
        (0, Some("ICE 273 InterCityExpress"), "273", "ICE 273", Some(ProductClass::LongDistance)),
        (0, Some("NJ 40420 nightjet"), "40420", "NJ 40420", Some(ProductClass::LongDistance)),
        (15, Some("IC 2023 InterCity"), "2023", "IC 2023", Some(ProductClass::LongDistance)),
        (0, Some("Zug 4711"), "4711", "Zug 4711", Some(ProductClass::Regional)),
        // The number is the line: kept as it is.
        (13, Some("R-Bahn RE2"), "RE2", "RE2", Some(ProductClass::Regional)),
        (0, Some("ICE InterCityExpress"), "273", "273", Some(ProductClass::Regional)),
        (0, None, "273", "273", Some(ProductClass::Regional)),
    ];
    for (class, name, number, label, db_class) in cases {
        let text = journey(&[(
            class,
            Some("2026-10-25T05:15:00Z"),
            Some("2026-10-25T07:00:00Z"),
            json!({ "transportation": { "id": "line", "name": name, "number": number, "product": { "class": class } } }),
        )]);
        let leg = &itineraries(&text)[0].legs[0];
        assert_eq!((leg.label(), leg.class()), (label.to_owned(), db_class), "{name:?}");
    }
}

#[test]
fn places_drop_unusable_coordinates_and_platforms() {
    let text = journey(&[(
        0,
        Some("2026-10-24T08:00:00Z"),
        Some("2026-10-24T08:30:00Z"),
        json!({
            "origin": { "name": "A", "departureTimePlanned": "2026-10-24T08:00:00Z", "coord": [1.0, 2.0, 3.0], "properties": { "platform": "" } },
            "transportation": { "product": { "class": 0 } },
        }),
    )]);
    let leg = &itineraries(&text)[0].legs[0];
    assert_eq!((leg.from.lat, leg.from.lon, leg.from.track.as_deref()), (None, None, None));
    assert!(leg.is_transit());
}

// ---- client ----

#[derive(Default)]
struct Fake {
    answers: Mutex<Vec<(StatusCode, String)>>,
    log: Mutex<Vec<HashMap<String, String>>>,
}

async fn answer(State(fake): State<Arc<Fake>>, Query(query): Query<HashMap<String, String>>) -> (StatusCode, String) {
    fake.log.lock().unwrap().push(query);
    let mut answers = fake.answers.lock().unwrap();
    if answers.len() > 1 { answers.remove(0) } else { answers[0].clone() }
}

async fn client(answers: &[(u16, &str)]) -> (Efa, Arc<Fake>) {
    let fake = Arc::new(Fake {
        answers: Mutex::new(answers.iter().map(|&(s, b)| (StatusCode::from_u16(s).unwrap(), b.to_owned())).collect()),
        log: Mutex::default(),
    });
    let router = Router::new().route("/vrn/XML_TRIP_REQUEST2", axum::routing::get(answer)).with_state(fake.clone());
    let base = testkit::serve(router).await;
    (Efa::new(&format!("{base}/vrn/"), "umstieg-test").unwrap(), fake)
}

#[tokio::test]
async fn trips_are_asked_in_german_local_time_and_cached() {
    let (efa, fake) = client(&[(200, TRIPS)]).await;
    let trips = efa.trips("de:07332:116", "de:08222:2417", at("22:30")).await.unwrap();
    assert_eq!(trips.len(), 4);
    let query = fake.log.lock().unwrap()[0].clone();
    let expected = [
        ("name_origin", "de:07332:116"),
        ("name_destination", "de:08222:2417"),
        ("itdDate", "20261025"),
        ("itdTime", "0030"),
        ("itdTripDateTimeDepArr", "dep"),
        ("outputFormat", "rapidJSON"),
    ];
    for (key, value) in expected {
        assert_eq!(query.get(key).map(String::as_str), Some(value), "{key}");
    }

    assert_eq!(efa.trips("de:07332:116", "de:08222:2417", at("22:30")).await.unwrap().len(), 4);
    assert_eq!(fake.log.lock().unwrap().len(), 1, "the same question is answered from the cache");
    efa.trips("de:07332:116", "de:08222:2417", at("06:00")).await.unwrap();
    let log = fake.log.lock().unwrap().clone();
    assert_eq!((log.len(), log[1]["itdDate"].as_str(), log[1]["itdTime"].as_str()), (2, "20261024", "0800"));
    assert_eq!(efa.request_count(), 2);
}

#[tokio::test]
async fn failures_are_retried_then_reported() {
    let (efa, fake) = client(&[(500, "busy"), (503, "busy"), (200, r#"{"version":"10","systemMessages":[]}"#)]).await;
    assert!(efa.trips("a", "b", at("08:00")).await.unwrap().is_empty(), "no journeys is no error");
    assert_eq!(fake.log.lock().unwrap().len(), 3);

    let (efa, fake) = client(&[(500, "busy")]).await;
    let error = efa.trips("a", "b", at("08:00")).await.unwrap_err();
    assert!(format!("{error:#}").contains("500"), "{error:#}");
    assert_eq!(fake.log.lock().unwrap().len(), ATTEMPTS as usize);

    let (efa, _) = client(&[(200, "<html>maintenance</html>")]).await;
    let error = efa.trips("a", "b", at("08:00")).await.unwrap_err();
    assert!(format!("{error:#}").contains("decoding"), "{error:#}");
}
