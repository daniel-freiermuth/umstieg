use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::*;
use crate::testkit::{self, at};

const PLAN: &str = include_str!("../testdata/motis/plan_neustadt_goeteborg.json");

fn leg(fields: &Value) -> Leg {
    let mut leg = json!({
        "mode": "REGIONAL_RAIL",
        "from": { "name": "A" },
        "to": { "name": "B" },
        "startTime": "2026-10-24T08:00:00Z",
        "endTime": "2026-10-24T09:30:00Z",
    });
    for (key, value) in fields.as_object().unwrap() {
        leg[key] = value.clone();
    }
    serde_json::from_value(leg).unwrap()
}

#[test]
fn labels_name_line_operator_and_train_number() {
    let cases = [
        (json!({ "displayName": "ICE 772", "tripShortName": "ICE 772" }), "ICE 772"),
        (json!({ "displayName": "ICE 616", "tripShortName": "616" }), "ICE 616"),
        (json!({ "displayName": "RE7", "tripShortName": "011174" }), "RE7 (11174)"),
        (json!({ "displayName": "RE8 (11406)", "tripShortName": "011406" }), "RE8 (11406)"),
        (json!({ "displayName": "RJ", "tripShortName": "000382", "agencyName": "DSB" }), "RJ (382)"),
        (
            json!({ "displayName": "804", "tripShortName": "1130", "agencyName": "Skånetrafiken" }),
            "Skånetrafiken 804 (1130)",
        ),
        (json!({ "displayName": "804" }), "804"),
        (json!({ "displayName": "NT", "tripShortName": "" }), "NT"),
        (json!({ "displayName": " S1 ", "routeShortName": "S2" }), "S1"),
        (json!({ "routeShortName": "S2" }), "S2"),
        (json!({ "displayName": "", "routeShortName": "S2" }), "S2"),
        (json!({ "displayName": " ", "mode": "BUS" }), "BUS"),
        (json!({ "mode": "COACH", "tripShortName": "42" }), "COACH (42)"),
        (
            json!({ "displayName": "Ø Göteborg-Malmö", "routeLongName": "Ø Göteborg-Malmö", "agencyName": "Öresundståg" }),
            "Öresundståg",
        ),
        (json!({ "displayName": "Ø Göteborg-Malmö", "routeLongName": "Ø Göteborg-Malmö" }), "Ø Göteborg-Malmö"),
        (json!({ "displayName": "RE7", "routeLongName": "Kiel - Hamburg", "agencyName": "DB" }), "RE7"),
    ];
    for (fields, expected) in cases {
        assert_eq!(leg(&fields).label(), expected, "{fields}");
    }
}

#[test]
fn train_numbers_are_the_trailing_digits() {
    let cases = [
        (Some("ICE 378"), Some(378)),
        (Some("011174"), Some(11174)),
        (Some("RJ"), None),
        (Some(""), None),
        (Some("0"), None),
        (Some("ICE 378a"), None),
        (Some("99999999999"), None),
        (None, None),
    ];
    for (name, expected) in cases {
        assert_eq!(leg(&json!({ "tripShortName": name })).train_number(), expected, "{name:?}");
    }
}

#[test]
fn a_train_without_trip_name_has_its_route_names_number() {
    // Trafiklab's Öresundståg 1036: route "1036", no trip name, mode LONG_DISTANCE.
    let cases = [
        (json!({ "mode": "LONG_DISTANCE", "routeShortName": "1036", "tripShortName": "" }), Some(1036)),
        (json!({ "mode": "REGIONAL_RAIL", "routeShortName": "1036" }), Some(1036)),
        (json!({ "mode": "LONG_DISTANCE", "routeShortName": "1036", "tripShortName": "ICE 5" }), Some(5)),
        (json!({ "mode": "BUS", "routeShortName": "150" }), None),
        (json!({ "mode": "REGIONAL_RAIL", "routeShortName": "RE7" }), None),
        (json!({ "mode": "REGIONAL_RAIL", "routeShortName": "" }), None),
    ];
    for (fields, expected) in cases {
        assert_eq!(leg(&fields).train_number(), expected, "{fields}");
    }
}

#[test]
fn classes_follow_db_product_classes() {
    let cases = [
        ("HIGHSPEED_RAIL", Some('F')),
        ("LONG_DISTANCE", Some('F')),
        ("NIGHT_RAIL", Some('F')),
        ("REGIONAL_FAST_RAIL", Some('N')),
        ("REGIONAL_RAIL", Some('N')),
        ("SUBURBAN", Some('S')),
        ("BUS", None),
        ("COACH", None),
        ("TRAM", None),
        ("WALK", None),
    ];
    for (mode, expected) in cases {
        assert_eq!(leg(&json!({ "mode": mode })).class(), expected, "{mode}");
    }
}

#[test]
fn legs_and_places_from_a_real_answer() {
    let plan: PlanResponse = serde_json::from_str(PLAN).unwrap();
    assert_eq!(plan.itineraries.len(), 6);
    let legs = &plan.itineraries[0].legs;
    let walks: Vec<&Leg> = legs.iter().filter(|l| !l.is_transit()).collect();
    assert!(walks.iter().all(|l| l.mode == "WALK"), "only walks lack a trip");
    let ice = legs.iter().find(|l| l.label() == "ICE 772").unwrap();
    assert_eq!((ice.class(), ice.train_number()), (Some('F'), Some(772)));
    assert_eq!(ice.duration(), ice.end_time - ice.start_time);
    assert!(ice.duration() > TimeDelta::hours(4));
    // Station ids: the parent station where the feed has one, else the stop.
    assert_eq!(ice.from.station_id(), Some("de-DELFI_de:08222:2417"));
    assert_ne!(ice.from.stop_id.as_deref(), ice.from.station_id());
    let ece = legs.iter().find(|l| l.label() == "ECE (394)").unwrap();
    assert_eq!(ece.from.station_id(), Some("dk-rejseplanen_000008020400"));
    assert_eq!(ece.from.parent_id, None);
}

// ---- client ----

type Answer = (StatusCode, Option<&'static str>, String);

/// Stand-in MOTIS: answers by path, in turn (the last one repeats); logs
/// every request's path and query.
#[derive(Default)]
struct Fake {
    answers: Mutex<HashMap<String, Vec<Answer>>>,
    geocode: Mutex<HashMap<String, Value>>,
    log: Mutex<Vec<(String, HashMap<String, String>)>>,
}

impl Fake {
    fn on(&self, path: &str, status: u16, retry_after: Option<&'static str>, body: &str) -> &Self {
        self.answers.lock().unwrap().entry(path.to_owned()).or_default().push((
            StatusCode::from_u16(status).unwrap(),
            retry_after,
            body.to_owned(),
        ));
        self
    }

    /// Geocoder answer for the query text `text`; other texts find nothing.
    fn geocode(&self, text: &str, matches: Value) -> &Self {
        self.geocode.lock().unwrap().insert(text.to_owned(), matches);
        self
    }

    fn texts(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().filter_map(|(_, q)| q.get("text").cloned()).collect()
    }

    fn requests(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

async fn answer(
    State(fake): State<Arc<Fake>>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let path = request.uri().path().to_owned();
    fake.log.lock().unwrap().push((path.clone(), query.clone()));
    if path == "/api/v1/geocode" && fake.answers.lock().unwrap().get(&path).is_none() {
        let matches = fake.geocode.lock().unwrap().get(&query["text"]).cloned().unwrap_or_else(|| json!([]));
        return axum::Json(matches).into_response();
    }
    let (status, retry_after, body) = {
        let mut answers = fake.answers.lock().unwrap();
        match answers.get_mut(&path) {
            Some(queue) if queue.len() > 1 => queue.remove(0),
            Some(queue) => queue[0].clone(),
            None => (StatusCode::NOT_FOUND, None, String::new()),
        }
    };
    match retry_after {
        Some(seconds) => (status, [(header::RETRY_AFTER, seconds)], body).into_response(),
        None => (status, body).into_response(),
    }
}

async fn client(fake: &Arc<Fake>) -> Motis {
    let base = testkit::serve(Router::new().fallback(answer).with_state(fake.clone())).await;
    Motis::new(&base, "umstieg-test").unwrap()
}

fn stop(kind: &str, name: &str, id: &str) -> Value {
    json!({ "type": kind, "name": name, "id": id, "lat": 49.46, "lon": 8.17 })
}

#[tokio::test]
async fn stop_retries_spellings_until_the_place_words_match() {
    let fake = Arc::new(Fake::default());
    fake.geocode("Bad Dürkheim Hbf", json!([stop("STOP", "Hauptbahnhof", "de-DELFI_de:08111:6115")])).geocode(
        "Bad Dürkheim Bahnhof",
        json!([
            stop("ADDRESS", "Bad Dürkheim Bahnhof", "address"),
            stop("STOP", "Bad Dürkheim Bahnhofstraße", "de-DELFI_de:07332:120"),
            stop("STOP", "Bad Dürkheim, Bahnhof", "de-DELFI_de:07332:116_G"),
        ]),
    );
    let motis = client(&fake).await;
    let found = motis.stop("Bad Dürkheim Hbf").await.unwrap();
    assert_eq!((found.id.as_str(), found.name.as_str()), ("de-DELFI_de:07332:116_G", "Bad Dürkheim, Bahnhof"));
    assert_eq!(found.position, Some((49.46, 8.17)));
    assert_eq!(fake.texts(), ["Bad Dürkheim Hbf", "Bad Dürkheim Bahnhof"], "original spelling, station words swapped");
}

#[tokio::test]
async fn stop_prefers_stations_in_geocoder_order() {
    let fake = Arc::new(Fake::default());
    fake.geocode(
        "Mannheim",
        json!([
            stop("STOP", "Mannheim Paradeplatz", "p"),
            stop("STOP", "Mannheim Hbf", "hbf"),
            stop("STOP", "Mannheim-Neckarau Bahnhof", "neckarau"),
        ]),
    )
    .geocode("Malmö", json!([stop("STOP", "Malmö Triangeln", "t"), stop("STOP", "Malmö C", "c")]))
    .geocode(
        "Neustadt Weinstr",
        json!([stop("STOP", "Neustadt (Holst)", "holst"), stop("STOP", "Neustadt (Weinstr) Hbf", "w")]),
    )
    .geocode("Koebenhavn H", json!([stop("STOP", "København H", "kh")]))
    .geocode("Paradeplatz", json!([stop("STOP", "Mannheim Paradeplatz", "p")]));
    let motis = client(&fake).await;
    let cases =
        [("Mannheim", "hbf"), ("Malmö", "c"), ("Neustadt Weinstr", "w"), ("Koebenhavn H", "kh"), ("Paradeplatz", "p")];
    for (text, id) in cases {
        assert_eq!(motis.stop(text).await.unwrap().id, id, "{text}");
    }
}

#[tokio::test]
async fn stop_matches_place_words_typed_without_accents() {
    let fake = Arc::new(Fake::default());
    fake.geocode("Koln Hbf", json!([stop("STOP", "Kiel Hbf", "kiel"), stop("STOP", "Köln Hbf", "koeln")]))
        .geocode("Zurich HB", json!([stop("STOP", "Zürich HB", "zh")]))
        .geocode("Geneve", json!([stop("STOP", "Genève", "ge")]))
        .geocode("Koln Sued", json!([stop("STOP", "Köln Süd", "sued")]));
    let motis = client(&fake).await;
    let cases = [("Koln Hbf", "koeln"), ("Zurich HB", "zh"), ("Geneve", "ge"), ("Koln Sued", "sued")];
    for (text, id) in cases {
        assert_eq!(motis.stop(text).await.unwrap().id, id, "{text}");
    }
}

#[test]
fn unaccented_drops_accents_and_folds_the_rest() {
    let cases = [
        ("Köln Hbf", "koln hbf"),
        ("Ålesund", "alesund"),
        ("Besançon-Viotte", "besancon viotte"),
        ("Saint-Étienne", "saint etienne"),
        ("Ïle", "ile"),
        ("Kraków Główny", "krakow glowny"),
        ("Plzeň", "plzen"),
        ("Přerov", "prerov"),
        ("Košice", "kosice"),
        ("Zürich HB", "zurich hb"),
        ("Vysoké Mýto", "vysoke myto"),
        ("Žilina", "zilina"),
        ("Gießen", "giessen"),
    ];
    for (input, expected) in cases {
        assert_eq!(unaccented(input), expected, "{input:?}");
    }
}

#[tokio::test]
async fn stop_prefers_where_trains_call_over_a_coach_stop_named_train_station() {
    let fake = Arc::new(Fake::default());
    let with_modes = |name: &str, id: &str, modes: &[&str]| {
        let mut s = stop("STOP", name, id);
        s["modes"] = json!(modes);
        s
    };
    fake.geocode(
        "Berlin Südkreuz",
        json!([
            with_modes("Berlin Südkreuz train station", "coach", &["COACH"]),
            with_modes("Berlin Südkreuz", "rail", &["LONG_DISTANCE", "BUS"]),
        ]),
    )
    .geocode(
        "Hamburg Hbf",
        json!([
            with_modes("Hamburg Hbf", "bus", &["BUS"]),
            with_modes("Hamburg Hbf", "s", &["SUBURBAN"]),
            with_modes("Hamburg Hbf", "ice", &["HIGHSPEED_RAIL"]),
        ]),
    );
    let motis = client(&fake).await;
    assert_eq!(motis.stop("Berlin Südkreuz").await.unwrap().id, "rail");
    assert_eq!(motis.stop("Hamburg Hbf").await.unwrap().id, "s", "any trains count, in the geocoder's order");
}

#[tokio::test]
async fn stop_ignores_connecting_words_missing_from_stop_names() {
    // Transitous' answers of 2026-10-30, abridged: the geocoder ranks queries with "am", "an der" badly.
    let fake = Arc::new(Fake::default());
    let with_modes = |name: &str, id: &str, modes: &[&str]| {
        let mut s = stop("STOP", name, id);
        s["modes"] = json!(modes);
        s
    };
    let main_hbf = |name: &str| with_modes(name, "main-hbf", &["LONG_DISTANCE", "SUBURBAN"]);
    let main_sued = || with_modes("Frankfurt (Main) Süd", "main-sued", &["LONG_DISTANCE", "SUBURBAN"]);
    for text in ["Frankfurt am Main Hbf", "Frankfurt am Main Hauptbahnhof"] {
        fake.geocode(text, json!([main_hbf("Frankfurt Hauptbahnhof"), main_sued()]));
    }
    fake.geocode(
        "Frankfurt am Main Bahnhof",
        json!([with_modes("Frankfurt (Main) Höchst Bahnhof", "hoechst", &["REGIONAL_RAIL"])]),
    )
    .geocode(
        "Frankfurt am Main",
        json!([
            with_modes("Frankfurt am Main, P+R", "p+r", &["RIDE_SHARING"]),
            main_hbf("Frankfurt (M) Hbf"),
            with_modes("Frankfurt (Main) Am Riedsteg", "riedsteg", &["BUS"]),
        ]),
    )
    .geocode("Frankfurt Main Hbf", json!([main_hbf("Frankfurt(Main) Hbf"), main_sued()]))
    .geocode(
        "Frankfurt an der Oder",
        json!([
            with_modes("Frankfurt (Oder), An den Seefichten", "seefichten", &["BUS"]),
            with_modes("Frankfurt (Main) An der Bornhohl", "bornhohl", &["BUS"]),
        ]),
    )
    .geocode(
        "Frankfurt Oder Hbf",
        json!([
            main_hbf("Frankfurt (M) Hbf"),
            with_modes("Frankfurt (Oder), Bahnhof", "oder", &["LONG_DISTANCE", "REGIONAL_RAIL"]),
        ]),
    );
    let motis = client(&fake).await;
    assert_eq!(motis.stop("Frankfurt am Main Hbf").await.unwrap().id, "main-hbf");
    assert_eq!(motis.stop("Frankfurt an der Oder Hbf").await.unwrap().id, "oder");
}

#[tokio::test]
async fn only_a_station_may_leave_out_the_connecting_words() {
    // Transitous' answers of 2026-10-31, abridged: without "an der", "am", "im" the
    // qualifier fits bus, U-Bahn and ride-sharing stops that name it by chance.
    let fake = Arc::new(Fake::default());
    let with_modes = |name: &str, id: &str, modes: &[&str]| {
        let mut s = stop("STOP", name, id);
        s["modes"] = json!(modes);
        s
    };
    let rrz = || with_modes("Mülheim Rhein-Ruhr-Zentrum", "rrz", &["SUBWAY", "BUS"]);
    let immenstadt = || with_modes("Immenstadt i. Allgäu,", "amarillo", &[]);
    fake.geocode("Mülheim Ruhr Hbf", json!([with_modes("Mülheim Hauptbahnhof", "hbf", &["REGIONAL_RAIL"])]))
        .geocode("Mülheim Ruhr Bahnhof", json!([rrz()]))
        .geocode("Mülheim Ruhr", json!([rrz(), with_modes("Mülheim Hochschule Ruhr West", "hrw", &["TRAM"])]))
        .geocode("Frankfurt Main West", json!([with_modes("Frankfurt (Main) Atzelberg West", "bus", &["BUS"])]))
        .geocode("Immenstadt Allgäu", json!([immenstadt()]))
        .geocode(
            "Immenstadt Allgäu Bahnhof",
            json!([with_modes("Immenstadt, Bahnhof", "bf", &["REGIONAL_RAIL"]), immenstadt()]),
        )
        .geocode("Freiburg im Breisgau Faigle", json!([with_modes("Freiburg im Breisgau, Faigle", "faigle", &[])]));
    let motis = client(&fake).await;
    for text in ["Mülheim an der Ruhr Hbf", "Frankfurt am Main West", "Immenstadt im Allgäu"] {
        let found = motis.stop(text).await.map(|s| s.id);
        assert!(found.is_err(), "{text}: found {found:?}");
    }
    assert_eq!(motis.stop("Freiburg im Breisgau Faigle").await.unwrap().id, "faigle", "named with \"im\"");
}

#[tokio::test]
async fn a_station_without_the_connecting_words_names_only_the_place() {
    // Transitous' answers of 2026-10-31, abridged: other stations name the qualifier too.
    let fake = Arc::new(Fake::default());
    let with_modes = |name: &str, id: &str, modes: &[&str]| {
        let mut s = stop("STOP", name, id);
        s["modes"] = json!(modes);
        s
    };
    let steinheim = || with_modes("Hanau-Steinheim (Main) Bahnhof", "steinheim", &["SUBURBAN"]);
    let hanau = || json!([with_modes("Hanau Hauptbahnhof", "hanau", &["LONG_DISTANCE"]), steinheim()]);
    fake.geocode(
        "Prien Chiemsee",
        json!([
            with_modes("Prien am Chiemsee", "prien", &["HIGHSPEED_RAIL", "REGIONAL_RAIL"]),
            with_modes("Stock Hafen Bahnhof, Prien a. Chiemsee", "stock", &["REGIONAL_RAIL"]),
        ]),
    )
    .geocode("Hanau Main Hbf", hanau())
    .geocode("Hanau Main Bahnhof", json!([steinheim()]))
    .geocode("Hanau Main Hauptbahnhof", hanau())
    .geocode("Hanau Main", json!([with_modes("Hanau Mainstraße", "bus", &["BUS"])]));
    let motis = client(&fake).await;
    let prien = motis.stop("Prien am Chiemsee").await.ok().map(|s| s.id);
    let hanau = motis.stop("Hanau am Main Hbf").await.ok().map(|s| s.id);
    assert_eq!((prien, hanau), (Some("prien".to_owned()), None), "Hanau's own station doesn't name \"Main\"");
}

#[tokio::test]
async fn a_stop_the_typed_name_finds_is_kept() {
    // Transitous' answers of 2026-10-31, abridged: without the connecting words the
    // geocoder names the same stops differently ("Rottenburg (Neckar)").
    let fake = Arc::new(Fake::default());
    let with_modes = |name: &str, id: &str, modes: &[&str]| {
        let mut s = stop("STOP", name, id);
        s["modes"] = json!(modes);
        s
    };
    let biberach = &["HIGHSPEED_RAIL", "REGIONAL_RAIL"];
    fake.geocode(
        "Rottenburg am Neckar",
        json!([
            with_modes("Rottenburg am Neckar, P+R", "p+r", &[]),
            with_modes("Rottenburg am Neckar, Bahnhof", "rottenburg", &["RIDE_SHARING"]),
        ]),
    )
    .geocode(
        "Rottenburg Neckar",
        json!([
            with_modes("Rottenburg (Neckar)", "rottenburg", &["RIDE_SHARING"]),
            with_modes("Rottenburg am Neckar, P+R", "p+r", &[]),
        ]),
    )
    .geocode(
        "Biberach an der Riß",
        json!([
            with_modes("Biberach an der Riß, Süd", "sued", &[]),
            with_modes("Biberach an der Riß, ZOB/Bahnhof", "biberach", biberach),
        ]),
    )
    .geocode(
        "Biberach Riß",
        json!([
            with_modes("Biberach (Riss), Bahnhof/ZOB", "biberach", biberach),
            with_modes("Biberach an der Riß, Süd", "sued", &[]),
        ]),
    )
    .geocode(
        "Lauffen am Neckar",
        json!([with_modes("Lauffen am Neckar,", "lauffen", &[]), with_modes("Lauffen am Neckar, P+R", "p+r", &[])]),
    )
    .geocode(
        "Lauffen Neckar",
        json!([with_modes("Lauffen (Neckar)", "lauffen", &[]), with_modes("Lauffen am Neckar, P+R", "p+r", &[])]),
    )
    .geocode("Esslingen am Neckar", json!([with_modes("Esslingen am Neckar,", "esslingen", &[])]))
    .geocode("Esslingen Neckar", json!([with_modes("Esslingen (Neckar)", "esslingen", &[])]))
    .geocode(
        "Esslingen Neckar Bahnhof",
        json!([
            with_modes("Esslingen am Neckar, Oberessl. Bahnhof (Süd)", "oberesslingen", &[]),
            with_modes("Esslingen (N)", "n", &["REGIONAL_RAIL"]),
        ]),
    );
    let motis = client(&fake).await;
    let mut found = Vec::new();
    for text in ["Rottenburg am Neckar", "Biberach an der Riß", "Lauffen am Neckar", "Esslingen am Neckar"] {
        found.push(motis.stop(text).await.ok().map(|s| s.id));
    }
    let expected = ["rottenburg", "biberach", "lauffen", "esslingen"].map(|id| Some(id.to_owned()));
    assert_eq!(found, expected);
}

#[tokio::test]
async fn a_station_found_on_asking_again_may_be_typed_without_accents() {
    // Transitous' answer of 2026-10-31, abridged: "Konigstein im Taunus" finds no stop naming "im".
    let fake = Arc::new(Fake::default());
    let mut station = stop("STOP", "Königstein (Taunus) Bahnhof", "koenigstein");
    station["modes"] = json!(["REGIONAL_RAIL", "BUS"]);
    fake.geocode("Konigstein Taunus", json!([station]));
    let motis = client(&fake).await;
    assert_eq!(motis.stop("Konigstein im Taunus").await.unwrap().id, "koenigstein");
}

#[tokio::test]
async fn text_without_connecting_words_is_asked_once() {
    let fake = Arc::new(Fake::default());
    let motis = client(&fake).await;
    let error = motis.stop("Atlantis Hbf").await.unwrap_err().to_string();
    assert_eq!(fake.texts().len(), 4, "{error}");
}

#[tokio::test]
async fn stop_tries_bare_place_last_and_reports_all_queries() {
    let fake = Arc::new(Fake::default());
    fake.geocode("Altona", json!([stop("STOP", "Hamburg-Altona", "altona")]));
    let motis = client(&fake).await;
    assert_eq!(motis.stop("Altona Bahnhof").await.unwrap().id, "altona");
    assert_eq!(fake.texts().last().map(String::as_str), Some("Altona"));
    assert!(fake.texts().contains(&"Altona Hauptbahnhof".to_owned()));

    let error = motis.stop("Atlantis Hbf").await.unwrap_err().to_string();
    assert!(error.contains("Atlantis Hbf") && error.contains("Atlantis Hauptbahnhof"), "{error}");
}

#[tokio::test]
async fn suggestions_are_unique_stop_names_at_most_eight_stops_with_trains_first() {
    let fake = Arc::new(Fake::default());
    let mut matches = vec![stop("ADDRESS", "Hamburg Straße", "a"), stop("STOP", "Hamburg Hbf", "1")];
    matches.push(stop("STOP", "Hamburg Hbf", "2"));
    matches.extend((0..10).map(|i| stop("STOP", &format!("Hamburg {i}"), &format!("x{i}"))));
    matches[5]["modes"] = json!(["REGIONAL_RAIL"]); // "Hamburg 2"
    fake.geocode("Hamb", Value::Array(matches));
    let motis = client(&fake).await;
    let found: Vec<(String, String)> =
        motis.suggest("Hamb").await.unwrap().into_iter().map(|s| (s.name, s.id)).collect();
    let expected = [
        ("Hamburg 2", "x2"),
        ("Hamburg Hbf", "1"),
        ("Hamburg 0", "x0"),
        ("Hamburg 1", "x1"),
        ("Hamburg 3", "x3"),
        ("Hamburg 4", "x4"),
        ("Hamburg 5", "x5"),
        ("Hamburg 6", "x6"),
    ];
    assert_eq!(found, expected.map(|(n, i)| (n.to_owned(), i.to_owned())));
    assert_eq!(motis.suggest("nothing").await.unwrap(), Vec::<Suggestion>::new());
}

#[tokio::test]
async fn a_picked_stop_is_found_by_its_id_among_those_of_its_name() {
    let fake = Arc::new(Fake::default());
    let mut second = stop("STOP", "Hamburg Hbf", "s");
    second["lat"] = json!(53.55);
    fake.geocode("Hamburg Hbf", json!([stop("STOP", "Hamburg Hbf", "bus"), second]));
    let motis = client(&fake).await;
    let found = motis.stop_by_id("Hamburg Hbf", "s").await.unwrap();
    assert_eq!((found.id.as_str(), found.position), ("s", Some((53.55, 8.17))));
    let error = motis.stop_by_id("Hamburg Hbf", "gone").await.unwrap_err().to_string();
    assert!(error.contains("no longer knows the station \"Hamburg Hbf\" (gone)"), "{error}");
}

#[tokio::test]
async fn plans_are_asked_once_then_cached_in_memory_and_on_disk() {
    let fake = Arc::new(Fake::default());
    fake.on("/api/v5/plan", 200, None, PLAN);
    let motis = client(&fake).await;
    let window = TimeDelta::hours(1);
    let first = motis.plan("from", "to", at("06:00"), window).await.unwrap();
    assert_eq!(first.len(), 6);
    let (_, query) = fake.log.lock().unwrap()[0].clone();
    let expected =
        [("fromPlace", "from"), ("toPlace", "to"), ("time", "2026-10-24T06:00:00Z"), ("searchWindow", "3600")];
    for (key, value) in expected {
        assert_eq!(query.get(key).map(String::as_str), Some(value), "{key}");
    }
    assert_eq!(motis.plan("from", "to", at("06:00"), window).await.unwrap().len(), 6);
    assert_eq!(fake.requests(), 1);
    assert_eq!(motis.request_count(), 1);

    motis.plan("from", "to", at("07:00"), window).await.unwrap();
    motis.plan("from", "to", at("06:00"), TimeDelta::hours(2)).await.unwrap();
    assert_eq!(fake.requests(), 3, "other times and windows are other queries");

    // A new client (another run) against the same server reads the disk cache.
    let again = Motis::new(&format!("{}/", motis.base), "umstieg-test").unwrap();
    assert_eq!(again.plan("from", "to", at("06:00"), window).await.unwrap().len(), 6);
    assert_eq!((fake.requests(), again.request_count()), (3, 0));

    // Another server is asked itself: cache keys include the base URL.
    let other = Arc::new(Fake::default());
    other.on("/api/v5/plan", 200, None, r#"{"itineraries":[]}"#);
    let elsewhere = client(&other).await;
    assert!(elsewhere.plan("from", "to", at("06:00"), window).await.unwrap().is_empty());
    assert_eq!(other.requests(), 1);
}

#[tokio::test]
async fn too_many_requests_are_retried_honouring_retry_after() {
    let fake = Arc::new(Fake::default());
    fake.on("/api/v1/geocode", 429, Some("1"), "").on("/api/v1/geocode", 200, None, "[]");
    let motis = client(&fake).await;
    let started = Instant::now();
    assert_eq!(motis.suggest("x").await.unwrap(), Vec::<Suggestion>::new());
    assert!(started.elapsed() >= std::time::Duration::from_secs(1), "waited as told");
    assert_eq!(fake.requests(), 2);

    let fake = Arc::new(Fake::default());
    for _ in 1..MAX_ATTEMPTS {
        fake.on("/api/v1/geocode", 429, None, "");
    }
    fake.on("/api/v1/geocode", 200, None, "[]");
    let motis = client(&fake).await;
    assert_eq!(motis.suggest("x").await.unwrap(), Vec::<Suggestion>::new());
    assert_eq!(fake.requests(), MAX_ATTEMPTS as usize, "every attempt but the last may be a 429");

    let fake = Arc::new(Fake::default());
    fake.on("/api/v1/geocode", 429, Some("0"), "");
    let motis = client(&fake).await;
    let error = motis.suggest("x").await.unwrap_err().to_string();
    assert!(error.contains("429"), "{error}");
    assert_eq!(fake.requests(), MAX_ATTEMPTS as usize);
}

#[tokio::test]
async fn server_errors_are_not_retried() {
    let fake = Arc::new(Fake::default());
    fake.on("/api/v5/plan", 500, None, "boom").on("/api/v5/plan", 200, None, PLAN);
    let motis = client(&fake).await;
    let error = motis.plan("a", "b", at("06:00"), TimeDelta::hours(1)).await.unwrap_err().to_string();
    assert!(error.contains("500"), "{error}");
    assert_eq!(fake.requests(), 1);
    // The failure isn't cached: asking again reaches the (now working) server.
    assert_eq!(motis.plan("a", "b", at("06:00"), TimeDelta::hours(1)).await.unwrap().len(), 6);
}

#[tokio::test]
async fn transport_errors_are_retried() {
    let (base, accepted) = testkit::serve_dropping(2, "200 OK", r#"[{"type":"STOP","name":"Fulda","id":"f"}]"#).await;
    let motis = Motis::new(&base, "umstieg-test").unwrap();
    assert_eq!(motis.suggest("Fulda").await.unwrap()[0].name, "Fulda");
    assert_eq!(accepted.load(Ordering::SeqCst), 3);

    let (base, _) = testkit::serve_dropping(usize::MAX, "200 OK", "").await;
    let motis = Motis::new(&base, "umstieg-test").unwrap();
    let error = motis.suggest("Fulda").await.unwrap_err();
    assert_eq!(error.to_string(), format!("GET {base}/api/v1/geocode"), "the last attempt's error: {error:#}");
    assert_eq!(motis.request_count(), MAX_ATTEMPTS as usize);
}

#[tokio::test]
async fn bodies_cut_short_are_retried() {
    let body = r#"[{"type":"STOP","name":"Fulda","id":"f"}]"#;
    let (base, accepted) = testkit::serve_faulty(testkit::Fault::Truncate, 2, "200 OK", body).await;
    let motis = Motis::new(&base, "umstieg-test").unwrap();
    assert_eq!(motis.suggest("Fulda").await.unwrap()[0].name, "Fulda");
    assert_eq!(accepted.load(Ordering::SeqCst), 3);

    let (base, _) = testkit::serve_faulty(testkit::Fault::Truncate, usize::MAX, "200 OK", body).await;
    let motis = Motis::new(&base, "umstieg-test").unwrap();
    let error = motis.suggest("Fulda").await.unwrap_err();
    assert_eq!(error.to_string(), format!("reading {base}/api/v1/geocode"), "the last attempt's error: {error:#}");
    assert_eq!(motis.request_count(), MAX_ATTEMPTS as usize);
}
