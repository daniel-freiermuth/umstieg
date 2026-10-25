use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Query as Params, State};
use axum::http::StatusCode;
use chrono::NaiveDateTime;
use chrono::TimeZone;
use chrono_tz::Europe::Berlin;
use serde_json::{Value, json};

use super::*;
use crate::testkit::{self, at};

const PLAN: &str = include_str!("../testdata/motis/plan_neustadt_goeteborg.json");

fn stop(id: &str, name: &str) -> Stop {
    Stop { id: id.to_owned(), name: name.to_owned(), position: None }
}

fn query(origin: Stop, destination: Stop, vias: Vec<Stop>, auto_hubs: usize) -> Query {
    Query { origin, destination, time: at("08:00"), window: TimeDelta::hours(2), vias, auto_hubs }
}

/// A MOTIS leg; `trip` None is a walk.
fn leg(trip: Option<&str>, from: (&str, &str), to: (&str, &str), start: &str, end: &str) -> Value {
    json!({
        "mode": if trip.is_some() { "REGIONAL_RAIL" } else { "WALK" },
        "from": { "stopId": from.0, "name": from.1 },
        "to": { "stopId": to.0, "name": to.1 },
        "startTime": at(start),
        "endTime": at(end),
        "tripId": trip,
        "displayName": trip,
    })
}

fn itinerary(legs: Vec<Value>) -> Itinerary {
    Itinerary { legs: legs.into_iter().map(|leg| serde_json::from_value(leg).unwrap()).collect() }
}

fn trips(itineraries: &[Itinerary]) -> BTreeSet<String> {
    itineraries.iter().flat_map(|i| &i.legs).filter_map(|l| l.trip_id.clone()).collect()
}

// ---- answers within the window ----

#[test]
fn departing_keeps_itineraries_whose_first_train_leaves_in_the_window() {
    let o = ("o", "Origin");
    let d = ("d", "Dest");
    let cases = [
        ("first minute", vec![leg(Some("t"), o, d, "08:00", "09:00")], true),
        ("last minute", vec![leg(Some("t"), o, d, "10:00", "11:00")], true),
        ("padded later", vec![leg(Some("t"), o, d, "10:01", "11:00")], false),
        ("before", vec![leg(Some("t"), o, d, "07:59", "09:00")], false),
        (
            "walk before the train",
            vec![leg(None, o, o, "07:50", "08:00"), leg(Some("t"), o, d, "08:00", "09:00")],
            true,
        ),
        (
            "walk in, train after",
            vec![leg(None, o, o, "09:50", "10:05"), leg(Some("t"), o, d, "10:05", "11:00")],
            false,
        ),
        ("walk only", vec![leg(None, o, d, "08:30", "09:00")], false),
    ];
    for (what, legs, kept) in cases {
        let kept_now = !departing(vec![itinerary(legs)], at("08:00"), TimeDelta::hours(2)).is_empty();
        assert_eq!(kept_now, kept, "{what}");
    }
}

// ---- hubs ----

fn fixture() -> Vec<Itinerary> {
    #[derive(serde::Deserialize)]
    struct Plan {
        itineraries: Vec<Itinerary>,
    }
    serde_json::from_str::<Plan>(PLAN).unwrap().itineraries
}

fn hub_names(hubs: &[Stop]) -> Vec<&str> {
    hubs.iter().map(|h| h.name.as_str()).collect()
}

#[test]
fn hubs_are_the_most_used_change_stations() {
    let origin = stop("de-DELFI_de:07316:3145", "Neustadt, Böbig");
    let destination = stop("se-Trafiklab_740000002", "Göteborg C");
    let hubs = pick_hubs(&fixture(), &query(origin.clone(), destination.clone(), Vec::new(), 3));
    // Mannheim: changes in all six answers; København H in four; Hamburg Hbf in three.
    assert_eq!(hub_names(&hubs), ["Mannheim, Hauptbahnhof", "København H", "Hamburg Hbf"]);
    assert_eq!(hubs[0].id, "de-DELFI_de:08222:2417", "station level: the parent id");
    assert_eq!(hubs[2].id, "de-DELFI_de:02000:10950", "of Hamburg Hbf's two ids, the more used one");

    // Hamburg Hbf's Danish id is skipped by name; ties go by id.
    let hubs = pick_hubs(&fixture(), &query(origin.clone(), destination.clone(), Vec::new(), 4));
    assert_eq!(hub_names(&hubs)[3], "Fulda Bahnhof");

    assert!(pick_hubs(&fixture(), &query(origin, destination, Vec::new(), 0)).is_empty());
}

#[test]
fn vias_come_first_and_count_on_top_of_auto_hubs() {
    let origin = stop("de-DELFI_de:07316:3145", "Neustadt, Böbig");
    let destination = stop("x", "København H");
    let vias = vec![stop("via-hh", "Hamburg Hbf"), stop("o2", "Neustadt, Böbig"), stop("via-hh2", "Hamburg Hbf")];
    let hubs = pick_hubs(&fixture(), &query(origin, destination, vias, 2));
    // The origin as via and the repeated via are dropped; the destination is never a hub.
    assert_eq!(hub_names(&hubs), ["Hamburg Hbf", "Mannheim, Hauptbahnhof", "Fulda Bahnhof"]);
    assert_eq!(hubs[0].id, "via-hh");
}

// ---- stand-in planners ----

/// Stand-in MOTIS: every itinerary of the asked pair whose first train leaves
/// at or after the asked time (MOTIS pads with later ones), except that,
/// like MOTIS, it drops a departure inside the search window if a later one
/// there arrives as early; pairs in `failing` answer 500.
#[derive(Default)]
struct FakeMotis {
    routes: Vec<((String, String), Value)>,
    failing: Vec<(String, String)>,
    log: Mutex<Vec<HashMap<String, String>>>,
}

impl FakeMotis {
    fn route(mut self, legs: Vec<Value>) -> Self {
        let from = legs[0]["from"]["stopId"].as_str().unwrap().to_owned();
        let to = legs[legs.len() - 1]["to"]["stopId"].as_str().unwrap().to_owned();
        let itinerary = Value::Object(std::iter::once(("legs".to_owned(), Value::Array(legs))).collect());
        self.routes.push(((from, to), itinerary));
        self
    }

    fn fail(mut self, from: &str, to: &str) -> Self {
        self.failing.push((from.to_owned(), to.to_owned()));
        self
    }

    /// (from, to, time, searchWindow) of the requests, sorted.
    fn asked(&self) -> Vec<(String, String, String, String)> {
        let mut asked: Vec<_> = self
            .log
            .lock()
            .unwrap()
            .iter()
            .map(|q| {
                let hhmm = q["time"].get(11..16).unwrap().to_owned();
                (q["fromPlace"].clone(), q["toPlace"].clone(), hhmm, q["searchWindow"].clone())
            })
            .collect();
        asked.sort();
        asked
    }
}

async fn plan(State(fake): State<Arc<FakeMotis>>, Params(q): Params<HashMap<String, String>>) -> (StatusCode, String) {
    fake.log.lock().unwrap().push(q.clone());
    let pair = (q["fromPlace"].clone(), q["toPlace"].clone());
    if fake.failing.contains(&pair) {
        return (StatusCode::INTERNAL_SERVER_ERROR, "down".to_owned());
    }
    let time: DateTime<Utc> = q["time"].parse().unwrap();
    let end = time + TimeDelta::seconds(q["searchWindow"].parse().unwrap());
    let span = |i: &Value| {
        let legs = i["legs"].as_array().unwrap();
        let parse = |t: &Value| t.as_str().unwrap().parse::<DateTime<Utc>>().unwrap();
        (parse(&legs[0]["startTime"]), parse(&legs[legs.len() - 1]["endTime"]))
    };
    let answers: Vec<&Value> =
        fake.routes.iter().filter(|(p, i)| *p == pair && span(i).0 >= time).map(|(_, i)| i).collect();
    let dominated = |i: &Value| {
        let (start, arrival) = span(i);
        start <= end
            && answers.iter().any(|j| {
                let (other_start, other_arrival) = span(j);
                other_start <= end && other_start > start && other_arrival <= arrival
            })
    };
    let itineraries: Vec<&Value> = answers.iter().copied().filter(|i| !dominated(i)).collect();
    (StatusCode::OK, json!({ "itineraries": itineraries }).to_string())
}

async fn fake_motis(fake: FakeMotis) -> (Motis, Arc<FakeMotis>) {
    let fake = Arc::new(fake);
    let base =
        testkit::serve(Router::new().route("/api/v5/plan", axum::routing::get(plan)).with_state(fake.clone())).await;
    (Motis::new(&base, "umstieg-test").unwrap(), fake)
}

/// Stand-in EFA: fixed journeys per (origin, destination) DHID pair, whatever
/// the time; pairs in `failing` answer 500.
#[derive(Default)]
struct FakeEfa {
    journeys: HashMap<(String, String), Vec<Value>>,
    failing: Vec<(String, String)>,
    log: Mutex<Vec<(String, String, DateTime<Utc>)>>,
}

impl FakeEfa {
    /// A tram journey `trip` (EFA trip code) from `from` to `to`.
    fn journey(mut self, from: &str, to: &str, trip: i64, departure: &str, arrival: &str) -> Self {
        let journey = json!({ "legs": [{
            "duration": 600,
            "origin": { "name": from, "id": from, "departureTimePlanned": at(departure) },
            "destination": { "name": to, "id": to, "arrivalTimePlanned": at(arrival) },
            "transportation": { "id": "tram", "number": "RNV 4", "product": { "class": 4 }, "properties": { "tripCode": trip } },
        }]});
        self.journeys.entry((from.to_owned(), to.to_owned())).or_default().push(journey);
        self
    }

    fn fail(mut self, from: &str, to: &str) -> Self {
        self.failing.push((from.to_owned(), to.to_owned()));
        self
    }

    fn asked(&self, from: &str, to: &str) -> Vec<DateTime<Utc>> {
        let mut times: Vec<_> =
            self.log.lock().unwrap().iter().filter(|(f, t, _)| f == from && t == to).map(|e| e.2).collect();
        times.sort();
        times.dedup();
        times
    }
}

async fn trip_request(
    State(fake): State<Arc<FakeEfa>>,
    Params(q): Params<HashMap<String, String>>,
) -> (StatusCode, String) {
    let pair = (q["name_origin"].clone(), q["name_destination"].clone());
    let local = NaiveDateTime::parse_from_str(&format!("{}{}", q["itdDate"], q["itdTime"]), "%Y%m%d%H%M").unwrap();
    let time = Berlin.from_local_datetime(&local).unwrap().with_timezone(&Utc);
    fake.log.lock().unwrap().push((pair.0.clone(), pair.1.clone(), time));
    if fake.failing.contains(&pair) {
        return (StatusCode::INTERNAL_SERVER_ERROR, "down".to_owned());
    }
    let journeys = fake.journeys.get(&pair).cloned().unwrap_or_default();
    (StatusCode::OK, json!({ "journeys": journeys }).to_string())
}

async fn fake_efa(fake: FakeEfa) -> (Efa, Arc<FakeEfa>) {
    let fake = Arc::new(fake);
    let router = Router::new().route("/XML_TRIP_REQUEST2", axum::routing::get(trip_request)).with_state(fake.clone());
    (Efa::new(&testkit::serve(router).await, "umstieg-test").unwrap(), fake)
}

// ---- slices and collection ----

#[tokio::test]
async fn windows_are_asked_in_hour_slices() {
    let (motis, fake) =
        fake_motis(FakeMotis::default().route(vec![leg(Some("t"), ("o", "O"), ("d", "D"), "09:30", "10:00")])).await;
    let found = sliced(&motis, "o", "d", at("08:00"), TimeDelta::minutes(150)).await.unwrap();
    let asked: Vec<(String, String)> = fake.asked().into_iter().map(|(_, _, t, w)| (t, w)).collect();
    let expected = [("08:00", "3600"), ("09:00", "3600"), ("10:00", "3600")].map(|(t, w)| (t.to_owned(), w.to_owned()));
    assert_eq!(asked, expected);
    assert_eq!(found.len(), 2, "unfiltered: the 08:00 and 09:00 slices both see the 09:30 train");

    assert!(sliced(&motis, "o", "d", at("08:00"), TimeDelta::zero()).await.unwrap().is_empty());
    assert_eq!(fake.asked().len(), 3, "an empty window asks nothing");
}

const O: (&str, &str) = ("o", "Origin");
const H: (&str, &str) = ("h", "Hub");
const X: (&str, &str) = ("x", "Elsewhere");
const D: (&str, &str) = ("d", "Dest");

/// Two answers changing at the hub inside 08:00–10:00 plus a padded one via X;
/// halves to and from the hub, some of them outside the windows. S1 is
/// dominated by B1 when both are in one search window.
fn corridor() -> FakeMotis {
    FakeMotis::default()
        .route(vec![
            leg(Some("A1"), O, H, "08:10", "08:40"),
            leg(None, H, H, "08:40", "08:45"),
            leg(Some("A2"), H, D, "09:00", "10:00"),
        ])
        .route(vec![leg(Some("B1"), O, H, "09:10", "09:40"), leg(Some("B2"), H, D, "10:00", "11:00")])
        .route(vec![leg(Some("C1"), O, X, "13:00", "13:30"), leg(Some("C2"), X, D, "13:40", "14:00")])
        .route(vec![leg(Some("A1"), O, H, "08:10", "08:40")])
        .route(vec![leg(Some("B1"), O, H, "09:10", "09:40")])
        .route(vec![leg(Some("E1"), O, H, "10:30", "11:00")])
        .route(vec![leg(Some("S1"), O, H, "08:20", "09:45")])
        .route(vec![leg(Some("A2"), H, D, "09:00", "10:00")])
        .route(vec![leg(Some("B2"), H, D, "10:00", "11:00")])
        .route(vec![leg(Some("L1"), H, D, "12:40", "13:30")])
        .route(vec![leg(Some("L2"), H, D, "12:46", "13:30")])
}

#[tokio::test]
async fn collect_adds_halves_through_hubs() {
    let (motis, fake) = fake_motis(corridor()).await;
    let q = query(stop("o", "Origin"), stop("d", "Dest"), Vec::new(), 2);
    let rough = collect(&motis, None, &q).await.unwrap();

    assert_eq!(rough.hubs, ["Hub", "Elsewhere"], "padded answers still show the corridor");
    assert!(rough.warnings.is_empty(), "{:?}", rough.warnings);
    // Not C1/C2 (padded), E1 (after the window), L2 (after the onward window).
    // S1 only shows up when origin → hub is asked hour by hour.
    let expected: BTreeSet<String> = ["A1", "A2", "B1", "B2", "L1", "S1"].map(str::to_owned).into();
    assert_eq!(trips(&rough.itineraries), expected);

    // Onward from the hub: from 08:00 (two hours before the 08:40 arrival,
    // but not before the query) to three hours after the last arrival (09:45).
    let onward: Vec<String> = fake.asked().into_iter().filter(|a| a.0 == "h").map(|a| a.2).collect();
    assert_eq!(onward, ["08:00", "09:00", "10:00", "11:00", "12:00"]);
    let to_x = fake.asked().into_iter().filter(|a| a.0 == "x").count();
    assert_eq!(to_x, 0, "nothing reaches X in the window: nothing onward is asked");
    let halves: Vec<String> =
        fake.asked().into_iter().filter(|a| a.1 != "d").map(|a| format!("{} {} {}", a.1, a.2, a.3)).collect();
    assert_eq!(halves, ["h 08:00 3600", "h 09:00 3600", "x 08:00 3600", "x 09:00 3600"], "hour slices");
}

#[tokio::test]
async fn failing_hubs_become_warnings_failing_baselines_errors() {
    let (motis, _) = fake_motis(corridor().fail("o", "x")).await;
    let q = query(stop("o", "Origin"), stop("d", "Dest"), Vec::new(), 2);
    let rough = collect(&motis, None, &q).await.unwrap();
    assert_eq!(rough.warnings.len(), 1);
    assert!(
        rough.warnings[0].starts_with("hub Elsewhere: ") && rough.warnings[0].contains("500"),
        "{:?}",
        rough.warnings
    );
    assert!(trips(&rough.itineraries).contains("L1"), "the other hub still counts");

    let (motis, _) = fake_motis(corridor().fail("o", "d")).await;
    assert!(collect(&motis, None, &q).await.is_err());
}

#[tokio::test]
async fn efa_is_asked_between_german_stops_and_its_failures_only_warn() {
    let origin = ("de-DELFI_de:07332:116_G", "Bad Dürkheim");
    let hub = ("de-DELFI_de:07314:2066", "LU Berliner Platz");
    let foreign = ("dk-rejseplanen_000008600626", "København H");
    let destination = ("de-DELFI_de:08222:2417", "Mannheim Hbf");
    let planner = FakeMotis::default()
        .route(vec![
            leg(Some("M1"), origin, hub, "08:10", "08:55"),
            leg(Some("M2"), hub, destination, "09:00", "09:10"),
        ])
        .route(vec![
            leg(Some("F1"), origin, foreign, "08:20", "08:50"),
            leg(Some("F2"), foreign, destination, "09:00", "09:30"),
        ])
        .route(vec![leg(Some("M1"), origin, hub, "08:10", "08:55")])
        .route(vec![leg(Some("M2"), hub, destination, "09:00", "09:10")]);
    let (motis, _) = fake_motis(planner).await;
    let (o, h, d) = ("de:07332:116", "de:07314:2066", "de:08222:2417");
    let (efa, fake) = fake_efa(
        FakeEfa::default()
            .journey(o, d, 1, "08:13", "09:05")
            .journey(o, d, 2, "11:13", "12:05")
            .fail(o, h)
            .journey(h, d, 3, "09:30", "09:40"),
    )
    .await;
    let q = query(stop(origin.0, origin.1), stop(destination.0, destination.1), Vec::new(), 2);
    let rough = collect(&motis, Some(&efa), &q).await.unwrap();

    assert_eq!(rough.hubs, ["LU Berliner Platz", "København H"]);
    let ids: Vec<&str> = rough.itineraries.iter().flat_map(|i| &i.legs).filter_map(|l| l.trip_id.as_deref()).collect();
    assert!(ids.contains(&"efa:tram:1:20261024"), "origin → destination journey in the window");
    assert!(!ids.contains(&"efa:tram:2:20261024"), "11:13 is after the window");
    assert!(ids.contains(&"efa:tram:3:20261024"), "hub → destination journey");
    assert_eq!(rough.warnings.len(), 2, "one per failed EFA request: {:?}", rough.warnings);
    assert!(rough.warnings.iter().all(|w| w.starts_with(&format!("EFA {o} → {h}: "))), "{:?}", rough.warnings);

    // Hourly across the window; never for the foreign hub.
    assert_eq!(fake.asked(o, d), [at("08:00"), at("09:00")]);
    assert_eq!(fake.asked(o, h), [at("08:00"), at("09:00")]);
    assert!(fake.log.lock().unwrap().iter().all(|(f, t, _)| f.starts_with("de:") && t.starts_with("de:")));
    let onward = fake.asked(h, d).len();
    assert_eq!(onward, 4, "08:00 until three hours after the 08:55 arrival");
    assert_eq!(fake.log.lock().unwrap().len(), 2 + 2 * 3 + onward, "failures are retried");
}
