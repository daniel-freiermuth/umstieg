use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use chrono::TimeDelta;

use super::*;
use crate::motis::Stop;
use crate::testkit;

const DEPARTURES: &str = include_str!("../testdata/sj/departures.json");

/// 2026-10-30 at "HH:MM" Swedish (winter) time.
fn swedish(hhmm: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&format!("2026-10-30T{hhmm}:00+01:00")).unwrap().to_utc()
}

fn fixture() -> Vec<Departure> {
    let answer: Answer = serde_json::from_str(DEPARTURES).unwrap();
    answer.travels.into_iter().flat_map(|t| t.departures).collect()
}

fn station(name: &str, position: (f64, f64)) -> trafikverket::Station {
    trafikverket::Station {
        signature: name.to_owned(),
        name: name.to_owned(),
        country: "SE".to_owned(),
        position: Some(position),
    }
}

/// Per leg: mode, label, train number, from → to stop ids, times.
fn legs(itinerary: &Itinerary) -> Vec<String> {
    itinerary
        .legs
        .iter()
        .map(|l| {
            let id = |p: &Place| p.stop_id.clone().unwrap_or_default().replace("se-Trafiklab_", "");
            let local = |t: DateTime<Utc>| t.with_timezone(&Stockholm).format("%H:%M").to_string();
            format!(
                "{} {} #{} {}→{} {}–{}",
                l.mode.as_str(),
                if l.is_transit() { l.label() } else { "-".to_owned() },
                l.train_number().map(|n| n.to_string()).unwrap_or_default(),
                id(&l.from),
                id(&l.to),
                local(l.start_time),
                local(l.end_time)
            )
        })
        .collect()
}

#[test]
fn uic_codes_from_transitous_stop_ids() {
    let cases = [
        ("se-Trafiklab_740000073", Some("740000073")),
        ("se-Trafiklab_740001537", Some("740001537")),
        ("dk-rejseplanen_000007400073", Some("740000073")),
        ("dk-rejseplanen_000008600626", None),
        ("se-Trafiklab_9022050000001", None),
        ("se-Trafiklab_74000007x", None),
        ("se-Trafiklab_640000073", None),
        ("no-Entur_NSR:Quay:623", None),
        ("de-DELFI_de:08222:2417", None),
        ("dk-rejseplanen_0000074000", None),
    ];
    for (id, expected) in cases {
        assert_eq!(uic(id).as_deref(), expected, "{id}");
    }
}

#[test]
fn connections_leaving_in_the_span_become_itineraries() {
    let stations = [station("Emmaboda", (56.63, 15.535)), station("Göteborg C", (57.709, 11.974))];
    let found = itineraries_of(&fixture(), swedish("06:00"), swedish("07:00"), &stations);
    let got: Vec<Vec<String>> = found.iter().map(legs).collect();
    assert_eq!(
        got,
        [
            vec![
                "REGIONAL_RAIL SJ Regional 319 #319 740000002→740000096 06:04–09:27",
                "REGIONAL_RAIL Krösatågen Öst 8625 #8625 740000096→740001537 09:34–10:12",
                "REGIONAL_RAIL Öresundståg 1077 #1077 740001537→740000073 10:51–11:36",
            ],
            vec![
                // From Göteborg Central to the bus terminal, then the bus.
                "WALK - # 740000002→740020483 06:55–07:00",
                "COACH Vy Bus4You 654 # 740020483→740072050 07:00–10:25",
                // Malmö C Norra Vallgatan to Malmö Central: the change (an hour), walked in 10 minutes.
                "WALK - # 740072050→740000003 10:25–10:35",
                "REGIONAL_RAIL Öresundståg 1052 #1052 740000003→740000073 11:27–13:21",
            ],
        ]
    );
    let sj = &found[0].legs[0];
    assert_eq!((sj.from.lat, sj.from.lon), (Some(57.709), Some(11.974)), "Trafikverket's position by name");
    assert_eq!((sj.to.lat, sj.to.lon), (Some(56.63), Some(15.535)));
    assert_eq!(found[0].legs[2].to.lat, None, "Karlshamn isn't in this station list");
    assert_eq!((sj.from.tz.as_deref(), sj.agency_name.as_deref()), (Some("Europe/Stockholm"), Some("SJ")));
    assert_eq!(sj.trip_id.as_deref().map(|t| t.starts_with("sj:")), Some(true));
    assert_ne!(sj.trip_id, found[0].legs[1].trip_id);
    assert_eq!(
        (sj.from.scheduled_departure, sj.to.scheduled_arrival),
        (Some(swedish("06:04")), Some(swedish("09:27")))
    );
}

#[test]
fn replacement_buses_trams_and_night_connections() {
    let all = itineraries_of(&fixture(), swedish("00:00"), swedish("23:59"), &[]);
    let modes: Vec<Vec<&str>> = all.iter().map(|i| i.legs.iter().map(|l| l.mode.as_str()).collect()).collect();
    assert_eq!(
        modes,
        [
            vec!["REGIONAL_RAIL", "BUS", "REGIONAL_RAIL", "REGIONAL_RAIL"],
            vec!["REGIONAL_RAIL", "REGIONAL_RAIL", "REGIONAL_RAIL"],
            vec!["WALK", "COACH", "WALK", "REGIONAL_RAIL"],
            vec!["WALK", "TRAM", "BUS", "COACH", "WALK", "REGIONAL_RAIL"],
        ]
    );
    // A bus's number is its line, not a train number to merge on.
    let bus = &all[0].legs[1];
    assert_eq!((bus.label(), bus.train_number()), ("Öresundståg 6804".to_owned(), None));
}

#[test]
fn dates_cover_the_span_in_swedish_time() {
    let day = |d: u32| NaiveDate::from_ymd_opt(2026, 10, d).unwrap();
    assert_eq!(dates(swedish("06:00"), swedish("08:00")), [day(30)]);
    // 23:30 UTC is already the next day in Sweden.
    assert_eq!(dates(swedish("22:00"), swedish("22:00") + TimeDelta::hours(3)), [day(30), day(31)]);
    assert_eq!(
        dates(swedish("06:00"), swedish("06:00") + TimeDelta::days(10)),
        [day(30), day(31), NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()]
    );
    assert_eq!(dates(swedish("06:00") + TimeDelta::days(1), swedish("06:00")), Vec::<NaiveDate>::new());
}

fn query(from: &str, to: &str) -> Query {
    let stop = |id: &str| Stop { id: id.to_owned(), name: id.to_owned(), position: None };
    Query {
        origin: stop(from),
        destination: stop(to),
        time: swedish("06:00"),
        window: TimeDelta::hours(2),
        vias: Vec::new(),
        auto_hubs: 0,
    }
}

/// A rough route calling at `stops` (stop id, departure "HH:MM" Swedish).
fn route(stops: &[(&str, &str)]) -> Itinerary {
    let place = |&(id, hhmm): &(&str, &str)| Place {
        name: id.to_owned(),
        stop_id: Some(id.to_owned()),
        parent_id: None,
        tz: None,
        track: None,
        lat: None,
        lon: None,
        arrival: None,
        departure: None,
        scheduled_arrival: None,
        scheduled_departure: (!hhmm.is_empty()).then(|| swedish(hhmm)),
        cancelled: false,
    };
    let places: Vec<Place> = stops.iter().map(place).collect();
    let leg = Leg {
        mode: Mode::RegionalRail,
        from: places[0].clone(),
        to: places[places.len().saturating_sub(1)].clone(),
        start_time: swedish("06:00"),
        end_time: swedish("10:00"),
        display_name: None,
        route_short_name: None,
        route_long_name: None,
        trip_id: Some("t".to_owned()),
        trip_short_name: None,
        headsign: None,
        agency_name: None,
        intermediate_stops: places[1..places.len().saturating_sub(1)].to_vec(),
    };
    Itinerary { legs: vec![leg] }
}

#[test]
fn the_swedish_ends_are_the_search_s_or_where_its_routes_cross_the_border() {
    let (g, kh) = ("se-Trafiklab_740000002", "se-Trafiklab_740000073");
    // Both ends Swedish: the search's window.
    let ends = swedish_ends(&query(g, kh), &[]).unwrap();
    assert_eq!(ends, (("740000002".to_owned(), swedish("06:00"), swedish("08:00")), "740000073".to_owned()));

    // From Hamburg: where the routes enter Sweden, mostly Malmö (Rejseplanen's
    // id), when they leave there; to Kalmar, the search's destination.
    let hamburg = "de-DELFI_de:02000:10950";
    let malmo = "dk-rejseplanen_000007400003";
    let routes = [
        route(&[(hamburg, "01:00"), ("dk-x", "04:00"), (malmo, "06:30"), ("se-Trafiklab_740000096", "09:00")]),
        route(&[(hamburg, "02:00"), (malmo, "07:15")]),
        route(&[(hamburg, "03:00"), ("se-Trafiklab_740000120", "07:40")]),
    ];
    let ends = swedish_ends(&query(hamburg, "se-Trafiklab_740000077"), &routes).unwrap();
    assert_eq!(ends, (("740000003".to_owned(), swedish("06:30"), swedish("07:15")), "740000077".to_owned()));
    // To Hamburg: where they leave Sweden.
    let back = [route(&[(g, "06:10"), ("se-Trafiklab_740000003", "09:00"), ("dk-x", "10:00"), (hamburg, "")])];
    let ends = swedish_ends(&query(g, hamburg), &back).unwrap();
    assert_eq!(ends.1, "740000003");

    // Not through Sweden, or only one Swedish stop: SJ isn't asked.
    let german = [route(&[(hamburg, "06:10"), ("de-DELFI_x", "08:00")])];
    assert_eq!(swedish_ends(&query(hamburg, "de-DELFI_x"), &german), None);
    assert_eq!(swedish_ends(&query(g, hamburg), &[route(&[(g, "06:10"), (hamburg, "")])]), None);
}

/// Stand-in for SJ's API: a search id for every search, the fixture for it;
/// what it was asked, and whether with SJ's key.
#[derive(Clone, Default)]
struct Fake {
    log: Arc<Mutex<Vec<String>>>,
}

async fn fake(log: Arc<Mutex<Vec<String>>>, fail: bool) -> String {
    async fn search(State(fake): State<Fake>, headers: HeaderMap, body: String) -> (StatusCode, String) {
        let key = headers.get("ocp-apim-subscription-key").and_then(|v| v.to_str().ok()) == Some(KEY);
        fake.log.lock().unwrap().push(format!("search {body} key={key}"));
        (StatusCode::OK, r#"{"departureSearchId":"abc"}"#.to_owned())
    }
    async fn departures(State(fake): State<Fake>, Path(id): Path<String>) -> String {
        fake.log.lock().unwrap().push(format!("departures {id}"));
        DEPARTURES.to_owned()
    }
    let state = Fake { log };
    let router = if fail {
        Router::new().fallback(|| async { (StatusCode::FORBIDDEN, "blocked".to_owned()) })
    } else {
        Router::new().route("/search", post(search)).route("/departures/search/{id}", get(departures))
    };
    testkit::serve(router.with_state(state)).await
}

#[tokio::test]
async fn sj_is_asked_once_per_day_and_route_with_its_key() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sj = Sj::new(&fake(log.clone(), false).await, "umstieg-test").unwrap();
    let day = NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
    assert_eq!(sj.departures("740000002", "740000073", day).await.unwrap().len(), 4);
    let asked = log.lock().unwrap().clone();
    assert_eq!(asked.len(), 2);
    let search: serde_json::Value =
        serde_json::from_str(asked[0].strip_prefix("search ").unwrap().strip_suffix(" key=true").unwrap()).unwrap();
    assert_eq!(
        (&search["origin"], &search["destination"], &search["departureDate"]),
        (&serde_json::json!("740000002"), &serde_json::json!("740000073"), &serde_json::json!("2026-10-30"))
    );
    assert_eq!(asked[1], "departures abc");
    // Cached: the same search again asks nothing; another day does.
    sj.departures("740000002", "740000073", day).await.unwrap();
    assert_eq!((log.lock().unwrap().len(), sj.request_count()), (2, 2));
    sj.departures("740000002", "740000073", day.succ_opt().unwrap()).await.unwrap();
    assert_eq!(log.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn rough_routes_from_sj_only_through_sweden() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sj = Sj::new(&fake(log.clone(), false).await, "umstieg-test").unwrap();
    let found = rough(&sj, None, &query("se-Trafiklab_740000002", "se-Trafiklab_740000073"), &[]).await.unwrap();
    // 06:00–08:00: the 06:04 and the 07:00.
    assert_eq!(found.len(), 2);
    let german = rough(&sj, None, &query("de-DELFI_a", "de-DELFI_b"), &[]).await.unwrap();
    assert!(german.is_empty() && sj.request_count() == 2, "not asked");
}

#[tokio::test]
async fn a_refusal_is_an_error() {
    let sj = Sj::new(&fake(Arc::default(), true).await, "umstieg-test").unwrap();
    let e = rough(&sj, None, &query("se-Trafiklab_740000002", "se-Trafiklab_740000073"), &[]).await.unwrap_err();
    assert!(format!("{e:#}").contains("403"), "{e:#}");
}

#[test]
fn modes_follow_sj_s_transport_methods() {
    let leg = |method: &str, modality: &str| -> SjLeg {
        serde_json::from_value(serde_json::json!({
            "origin": { "uicStationCode": "740000002", "name": "A" },
            "destination": { "uicStationCode": "740000003", "name": "B" },
            "departureDateTime": "2026-10-30T06:00:00+01:00",
            "arrivalDateTime": "2026-10-30T07:00:00+01:00",
            "serviceName": "483",
            "transportMethod": method,
            "serviceType": { "modality": modality, "description": "x" },
        }))
        .unwrap()
    };
    let cases = [
        ("TRAIN", "HIGHSPEED_RAIL", "HIGHSPEED_RAIL"),
        ("TRAIN", "REGIONAL_RAIL_SERVICE", "REGIONAL_RAIL"),
        ("EXPRESSBUS", "EXPRESS_BUS_SERVICE", "COACH"),
        ("TRAM", "TRAM_SERVICE", "TRAM"),
        ("FERRY", "FERRY_SERVICE", "FERRY"),
        ("BUS", "HIGHSPEED_RAIL", "BUS"),
        ("TAXI", "", "BUS"),
    ];
    for (method, modality, mode) in cases {
        assert_eq!(ride(&leg(method, modality), &[]).mode.as_str(), mode, "{method} {modality}");
    }
}
