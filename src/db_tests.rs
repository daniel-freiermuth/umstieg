use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use chrono::{DateTime, TimeZone, Utc};

use super::*;
use crate::testkit::{self, at};

const PLAN: &str = include_str!("../testdata/db/plan_8000244_261024_08.xml");
const CHANGES: &str = include_str!("../testdata/db/fchg_8000244.xml");

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
}

fn stations_xml(stations: &[(&str, &str)]) -> String {
    let body = stations
        .iter()
        .map(|(name, eva)| format!(r#"<station p="" meta="" name="{name}" eva="{eva}" ds100="X" db="true"/>"#))
        .collect::<Vec<_>>()
        .concat();
    format!("<stations>{body}</stations>")
}

/// Stand-in for the DB API: answers by request path (percent-encoded, as
/// sent), each path's answers in turn with the last one repeating; 404 for
/// unknown paths, 401 without the test credentials.
#[derive(Default)]
struct Fake {
    answers: Mutex<HashMap<String, Vec<(StatusCode, String)>>>,
    log: Mutex<Vec<String>>,
}

impl Fake {
    fn on(&self, path: &str, status: u16, body: &str) -> &Self {
        self.answers
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .or_default()
            .push((StatusCode::from_u16(status).unwrap(), body.to_owned()));
        self
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn count(&self, prefix: &str) -> usize {
        self.log().iter().filter(|p| p.starts_with(prefix)).count()
    }
}

async fn answer(State(fake): State<Arc<Fake>>, request: Request) -> (StatusCode, String) {
    let header = |name: &str| request.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    if header("DB-Client-Id").as_deref() != Some("id") || header("DB-Api-Key").as_deref() != Some("key") {
        return (StatusCode::UNAUTHORIZED, "credentials missing".to_owned());
    }
    let path = request.uri().path().to_owned();
    fake.log.lock().unwrap().push(path.clone());
    let mut answers = fake.answers.lock().unwrap();
    match answers.get_mut(&path) {
        Some(queue) if queue.len() > 1 => queue.remove(0),
        Some(queue) => queue[0].clone(),
        None => (StatusCode::NOT_FOUND, String::new()),
    }
}

async fn client(fake: &Arc<Fake>) -> Db {
    let base = testkit::serve(Router::new().fallback(answer).with_state(fake.clone())).await;
    Db::new(&base, Credentials { client_id: "id".to_owned(), api_key: "key".to_owned() }, "umstieg-test").unwrap()
}

fn by_label(visits: &[Visit]) -> HashMap<String, &Visit> {
    visits.iter().map(|v| (v.label(), v)).collect()
}

// ---- parsing ----

#[test]
fn plan_board_parses_trains_times_platforms_and_paths() {
    let visits = parse_visits(PLAN).unwrap();
    assert_eq!(visits.len(), 6);
    for (id, visit) in &visits {
        assert_eq!(*id, visit_id(visit), "the id splits into run and position");
    }
    let visits: Vec<Visit> = visits.into_iter().map(|(_, v)| v).collect();
    let labels = by_label(&visits);

    let ice = labels["ICE 591"];
    assert_eq!(ice.trip, "4306741898969377109-2610240529");
    assert_eq!(ice.seq, 5);
    assert_eq!(
        (ice.number, ice.class, ice.category.as_deref(), ice.line.as_deref()),
        (Some(591), Some('F'), Some("ICE"), None)
    );
    let arrival = ice.arrival.as_ref().unwrap();
    assert_eq!(arrival.planned, Some(at("06:30")), "08:30 CEST");
    assert_eq!(arrival.changed, None);
    assert_eq!(arrival.platform(), Some("5"));
    assert_eq!(arrival.path(), ["Erfurt Hbf", "Eisenach Hbf", "Fulda", "Frankfurt(Main)Süd"]);
    assert_eq!(ice.departure.as_ref().unwrap().planned, Some(at("06:33")));

    // Negative run ids keep their sign; the line comes from the events.
    let s1 = labels["S1 (38114)"];
    assert_eq!(s1.trip, "-5139865393715302214-2610240636");
    assert_eq!((s1.seq, s1.class), (32, Some('S')));

    // Trains ending here have no departure, trains starting here no arrival.
    let ending = labels["RE70 (4557)"];
    assert!(ending.arrival.is_some() && ending.departure.is_none());
    assert_eq!(ending.arrival.as_ref().unwrap().platform(), Some("7 A-C"));
    let starting = labels["S3 (38314)"];
    assert!(starting.arrival.is_none() && starting.departure.is_some());

    // Foreign trains may lack the product class: long-distance categories give it.
    let ece = labels["ECE 151"];
    assert_eq!((ece.class, ece.number), (Some('F'), Some(151)));
}

#[test]
fn trains_without_class_or_with_a_bare_line_number_get_category_labels() {
    let board = |tl: &str, line: &str| {
        let xml = format!(r#"<timetable><s id="7-2610260615-4">{tl}<dp pt="2610260758" l="{line}"/></s></timetable>"#);
        parse_visits(&xml).unwrap().remove(0).1
    };
    // FlixTrain: no class, the line is "10".
    let flx = board(r#"<tl t="p" o="FLX10" c="FLX" n="1236"/>"#, "10");
    assert_eq!((flx.label(), flx.class), ("FLX 1236".to_owned(), Some('F')));
    // A line with letters names the train as before; an unknown category stays without class.
    let re = board(r#"<tl f="N" t="p" o="800" c="RE" n="17005"/>"#, "RE7");
    assert_eq!((re.label(), re.class), ("RE7 (17005)".to_owned(), Some('N')));
    // S-Bahn lines also come as bare numbers: S1 keeps its line, not just its run number.
    let s1 = board(r#"<tl f="S" t="p" o="800244" c="S" n="34459"/>"#, "1");
    assert_eq!((s1.label(), s1.class), ("1 (34459)".to_owned(), Some('S')));
    let bus = board(r#"<tl t="p" o="X" c="Bus" n="4711"/>"#, "");
    assert_eq!((bus.label(), bus.class), ("Bus 4711".to_owned(), None));
}

#[test]
fn events_distinguish_planned_and_changed_data() {
    let doc = roxmltree::Document::parse(
        r#"<x>
            <ar pt="2610240830" ct="2610240842" pp="" cp="7" ppth="A||B" cpth="" cs="c"/>
            <dp pt="2610240833" pp="5" ppth="C|D" cs="p"/>
            <ar/>
        </x>"#,
    )
    .unwrap();
    let events: Vec<Event> =
        doc.root_element().children().filter(roxmltree::Node::is_element).map(parse_event).collect();

    let changed = &events[0];
    assert_eq!((changed.planned, changed.changed), (Some(at("06:30")), Some(at("06:42"))));
    assert_eq!(changed.planned_platform, None, "empty platform is none");
    assert_eq!(changed.platform(), Some("7"));
    assert_eq!(changed.planned_path, ["A", "B"], "empty path entries dropped");
    assert_eq!(changed.changed_path, Some(Vec::new()), "a changed path can be empty");
    assert!(changed.path().is_empty(), "the changed path wins even when empty");
    assert!(changed.cancelled);

    let planned = &events[1];
    assert!(!planned.cancelled, "only cs=c cancels");
    assert_eq!(planned.platform(), Some("5"));
    assert_eq!(planned.path(), ["C", "D"]);

    let bare = &events[2];
    assert_eq!((bare.planned, bare.changed, bare.platform()), (None, None, None));
    assert_eq!(bare.path(), Vec::<String>::new());
}

#[test]
fn times_are_berlin_local_across_daylight_saving_changes() {
    let cases = [
        ("2610240830", Some(utc(2026, 10, 24, 6, 30))), // CEST, UTC+2
        ("2610260830", Some(utc(2026, 10, 26, 7, 30))), // CET, UTC+1
        ("2610250159", Some(utc(2026, 10, 24, 23, 59))),
        // 02:30 happens twice on 25 October: the earlier (CEST) one.
        ("2610250230", Some(utc(2026, 10, 25, 0, 30))),
        ("2610250300", Some(utc(2026, 10, 25, 2, 0))),
        ("2603290130", Some(utc(2026, 3, 29, 0, 30))),
        ("2603290330", Some(utc(2026, 3, 29, 1, 30))),
        ("2612312359", Some(utc(2026, 12, 31, 22, 59))),
        ("", None),
        ("26102408", None),
        ("2613240830", None),
    ];
    for (text, expected) in cases {
        assert_eq!(parse_time(text), expected, "{text:?}");
    }
}

#[test]
fn hour_slots_are_berlin_local_hours() {
    let cases = [
        (utc(2026, 10, 24, 6, 0), "261024/08"),
        (utc(2026, 10, 24, 5, 59), "261024/07"),
        (utc(2026, 10, 24, 22, 0), "261025/00"),
        // Both 02:xx hours of the DST change night share one slot.
        (utc(2026, 10, 25, 0, 30), "261025/02"),
        (utc(2026, 10, 25, 1, 30), "261025/02"),
        (utc(2026, 10, 25, 2, 0), "261025/03"),
        (utc(2026, 10, 26, 7, 0), "261026/08"),
    ];
    for (t, expected) in cases {
        assert_eq!(slot(t), expected, "{t}");
    }
}

#[test]
fn merging_a_change_keeps_the_plan_and_applies_live_data() {
    let mut plan = Visit {
        trip: "1-2610240529".to_owned(),
        seq: 5,
        number: Some(591),
        class: Some('F'),
        category: Some("ICE".to_owned()),
        line: None,
        arrival: Some(Event {
            planned: Some(at("06:30")),
            planned_platform: Some("5".to_owned()),
            planned_path: vec!["Fulda".to_owned()],
            ..Event::default()
        }),
        departure: None,
    };
    let change = Visit {
        trip: "1-2610240529".to_owned(),
        seq: 5,
        number: Some(1),
        class: Some('N'),
        category: Some("RE".to_owned()),
        line: Some("RE7".to_owned()),
        arrival: Some(Event {
            planned: Some(at("06:31")),
            changed: Some(at("06:42")),
            changed_platform: Some("6".to_owned()),
            cancelled: true,
            changed_path: Some(vec!["Kassel".to_owned()]),
            ..Event::default()
        }),
        departure: Some(Event { changed: Some(at("06:45")), ..Event::default() }),
    };
    plan.merge(&change);

    assert_eq!((plan.number, plan.class, plan.category.as_deref()), (Some(591), Some('F'), Some("ICE")), "plan wins");
    assert_eq!(plan.line.as_deref(), Some("RE7"), "missing fields are filled in");
    let arrival = plan.arrival.as_ref().unwrap();
    assert_eq!(arrival.planned, Some(at("06:30")), "planned time from the plan");
    assert_eq!(arrival.changed, Some(at("06:42")));
    assert_eq!(arrival.planned_platform.as_deref(), Some("5"));
    assert_eq!(arrival.platform(), Some("6"));
    assert!(arrival.cancelled);
    assert_eq!(arrival.planned_path, ["Fulda"]);
    assert_eq!(arrival.path(), ["Kassel"]);
    let departure = plan.departure.as_ref().unwrap();
    assert_eq!((departure.planned, departure.changed), (None, Some(at("06:45"))), "events the plan lacks are added");

    // A later change without new data keeps the earlier live data.
    let mut event = arrival.clone();
    event.merge(&Event::default());
    assert_eq!((event.changed, event.platform(), event.cancelled), (Some(at("06:42")), Some("6"), true));
    assert_eq!(event.path(), ["Kassel"]);
}

#[test]
fn labels_prefer_line_then_category() {
    let visit = |line: Option<&str>, category: Option<&str>, number: Option<u32>| Visit {
        line: line.map(str::to_owned),
        category: category.map(str::to_owned),
        number,
        ..Visit::default()
    };
    let cases = [
        (visit(Some("RE7"), Some("RE"), Some(11166)), "RE7 (11166)"),
        (visit(None, Some("ICE"), Some(378)), "ICE 378"),
        (visit(None, None, Some(378)), "378"),
    ];
    for (visit, expected) in cases {
        assert_eq!(visit.label(), expected);
    }
}

// ---- station names ----

#[test]
fn station_patterns_try_db_spellings_first() {
    let cases: [(&str, Option<&str>, &[&str]); 9] = [
        ("Mannheim Hauptbahnhof", None, &["Mannheim Hbf", "Mannheim Hauptbahnhof"]),
        ("Hamburg, Hauptbahnhof", None, &["Hamburg Hbf", "Hamburg, Hauptbahnhof"]),
        ("Fulda Bahnhof", None, &["Fulda", "Fulda Bahnhof"]),
        ("Bad Dürkheim, Bahnhof", None, &["Bad Dürkheim", "Bad Dürkheim, Bahnhof"]),
        (
            "Frankfurt (Main) Südbahnhof",
            None,
            &["Frankfurt(Main)Süd", "Frankfurt(M) Süd", "Frankfurt (Main) Südbahnhof"],
        ),
        (
            "Frankfurt (Main) Flughafen Fernbahnhof",
            None,
            &[
                "Frankfurt(Main)Flughafen Fernbahnhof",
                "Frankfurt(M) Flughafen Fernbf",
                "Frankfurt (Main) Flughafen Fernbahnhof",
            ],
        ),
        ("Hamburg Hbf", Some("de-DELFI_de:02000:10950"), &["Hamburg Hbf"]),
        ("København H", Some("dk-rejseplanen_000008600626"), &["8600626", "København H"]),
        ("Malmö C", Some("se-Trafiklab_740000003"), &["Malmö C"]),
    ];
    for (name, stop_id, expected) in cases {
        assert_eq!(station_patterns(name, stop_id), expected, "{name} / {stop_id:?}");
    }
}

#[test]
fn fold_makes_spellings_comparable() {
    let cases = [
        ("København H", "koebenhavn h"),
        ("Koebenhavn H", "koebenhavn h"),
        ("Göteborg C", "goeteborg c"),
        ("Malmö  C", "malmoe c"),
        ("Ærø", "aeroe"),
        ("Århus", "aarhus"),
        ("Straße", "strasse"),
        ("Frankfurt(Main)Süd", "frankfurt main sued"),
        ("Fulda-Horas", "fulda horas"),
        ("  Hamburg, Hbf. ", "hamburg hbf"),
        ("", ""),
    ];
    for (input, expected) in cases {
        assert_eq!(fold(input), expected, "{input:?}");
    }
}

#[test]
fn station_keys_match_the_names_feeds_give_one_station() {
    let same = [
        ("Göteborg Central", "Göteborg C"),
        ("Malmö Centralstation", "Malmö C"),
        ("Oslo Sentralstasjon", "Oslo Sentralstasjon"),
        ("Emmaboda station", "Emmaboda"),
        ("Karlshamn Resecentrum", "Karlshamn"),
        ("Landskrona St.", "Landskrona"),
        ("Landskrona St.", "Landskrona station"),
        ("Alvesta stn", "Alvesta"),
        ("Drammen stasjon", "Drammen"),
        ("Stockholm Centralen", "Stockholm C"),
        (" Bergåsa ", "Bergåsa"),
    ];
    for (a, b) in same {
        assert_eq!(station_key(a), station_key(b), "{a} / {b}");
    }
    let different = [
        ("Malmö C Norra Vallgatan", "Malmö C"),
        ("Karlshamn centrum väst", "Karlshamn"),
        ("Hamburg Hbf", "Hamburg-Altona"),
        ("Lund C", "Lund"),
        // Only a whole last word counts.
        ("Ost", "O"),
        ("Bergåsa", "Bergå"),
    ];
    for (a, b) in different {
        assert_ne!(station_key(a), station_key(b), "{a} / {b}");
    }
}

#[test]
fn same_place_needs_the_first_word() {
    let cases = [
        ("Frankfurt(M) Flughafen Fernbf", "Frankfurt (Main) Flughafen Fernbahnhof", true),
        ("Fulda-Horas", "Fulda", true),
        ("Hamburg Hbf", "Hamburg", true),
        ("Hamburg", "Hamburg-Altona", true),
        ("København H", "Koebenhavn H", true),
        // Either way round is enough.
        ("Bad Dürkheim", "Dürkheim", true),
        ("Dürkheim", "Bad Dürkheim", true),
        ("Mannheim Hbf", "Heidelberg Hbf", false),
        ("Neustadt(Weinstr)Hbf", "Bad Dürkheim", false),
        ("", "Fulda", false),
        ("Fulda", "", false),
        ("Fulda", "--", false),
    ];
    for (db, place, expected) in cases {
        assert_eq!(same_place(db, place), expected, "{db:?} vs {place:?}");
    }
}

#[test]
fn eva_numbers_in_stop_ids() {
    let cases = [
        ("dk-rejseplanen_000008600626", Some("8600626")),
        ("dk-rejseplanen_000008020400", Some("8020400")),
        ("8000244", Some("8000244")),
        ("de-DELFI_000008601770", Some("8601770")),
        // Swedish ids (74…), too long, too short, digits not at the end.
        ("dk-rejseplanen_000007400002", None),
        ("se-Trafiklab_740000002", None),
        ("x_80002440", None),
        ("de-DELFI_de:02000:10950", None),
        ("de-DELFI_de:08222:2417:5:5", None),
        ("8000244_G", None),
        ("", None),
    ];
    for (id, expected) in cases {
        assert_eq!(embedded_eva(id).as_deref(), expected, "{id:?}");
    }
}

#[test]
fn rail_evas_have_seven_digits_starting_with_8() {
    let cases = [
        ("8000244", true),
        ("8600626", true),
        ("210303", false),
        ("7400002", false),
        ("80002445", false),
        ("800024a", false),
        ("", false),
    ];
    for (eva, expected) in cases {
        assert_eq!(is_rail_eva(eva), expected, "{eva:?}");
    }
}

#[test]
fn urlencode_keeps_names_in_one_path_segment() {
    let cases = [
        ("Frankfurt(Main)Süd", "Frankfurt(Main)S%C3%BCd"),
        ("Frankfurt(M) Süd", "Frankfurt(M)%20S%C3%BCd"),
        ("Köln Messe/Deutz", "K%C3%B6ln%20Messe%2FDeutz"),
        ("a?b#c&d%e+f", "a%3Fb%23c%26d%25e%2Bf"),
        ("Halle(Saale)Hbf-Gl._1", "Halle(Saale)Hbf-Gl._1"),
    ];
    for (input, expected) in cases {
        assert_eq!(urlencode(input), expected, "{input:?}");
    }
}

#[test]
fn station_search_answers_keep_rail_stations_only() {
    let xml = stations_xml(&[("Altona Bahnhof (McDonald), Hamburg", "210303"), ("Hamburg-Altona", "8002553")]);
    let stations = parse_stations(&xml).unwrap();
    assert_eq!(stations.len(), 1);
    assert_eq!((stations[0].eva.as_str(), stations[0].name.as_str()), ("8002553", "Hamburg-Altona"));
    assert!(parse_stations("<stations><station").is_err());
}

#[test]
fn station_cache_lines() {
    let text = "Jungfernstieg\t\t\n\
                Schleswig\t8005362\tSchleswig\n\
                Altona\t210303\tAltona Bahnhof (McDonald), Hamburg\n\
                not a cache line\n\
                Kassel\t\t\n\
                Kassel\t8000193\tKassel Hbf\n";
    let cache = parse_station_cache(text);
    assert!(matches!(cache.get("Jungfernstieg"), Some(None)), "negative entries are kept");
    let schleswig = cache["Schleswig"].as_ref().unwrap();
    assert_eq!((schleswig.eva.as_str(), schleswig.name.as_str()), ("8005362", "Schleswig"));
    assert!(!cache.contains_key("Altona"), "bus stops from older versions are looked up again");
    assert!(!cache.contains_key("not a cache line"));
    assert_eq!(cache["Kassel"].as_ref().map(|s| s.eva.as_str()), Some("8000193"), "the latest line wins");
    assert_eq!(cache.len(), 3);
}

// ---- client ----

#[tokio::test]
async fn resolve_prefers_the_exact_name() {
    let fake = Arc::new(Fake::default());
    fake.on("/station/Fulda", 200, &stations_xml(&[("Fulda-Horas", "8002048"), ("Fulda", "8000115")]));
    let db = client(&fake).await;
    let station = db.resolve("Fulda Bahnhof", None).await.unwrap().unwrap();
    assert_eq!((station.eva.as_str(), station.name.as_str()), ("8000115", "Fulda"));
    assert_eq!(fake.log(), ["/station/Fulda"]);
}

#[tokio::test]
async fn resolve_falls_back_to_the_same_place_skipping_bus_stops_and_other_places() {
    let fake = Arc::new(Fake::default());
    fake.on(
        "/station/Testheim",
        200,
        &stations_xml(&[("Testheim ZOB", "123456"), ("Neustadt(Weinstr)Hbf", "8000276"), ("Testheim-Nord", "8000998")]),
    );
    let db = client(&fake).await;
    let station = db.resolve("Testheim Bahnhof", None).await.unwrap().unwrap();
    assert_eq!(station.eva, "8000998");
}

#[tokio::test]
async fn resolve_tries_patterns_in_order_starting_with_the_embedded_eva() {
    let fake = Arc::new(Fake::default());
    fake.on("/station/Frankfurt(M)%20S%C3%BCd", 200, &stations_xml(&[("Frankfurt(M) Süd", "8002041")]));
    fake.on("/station/8600626", 200, &stations_xml(&[("København H", "8600626")]));
    let db = client(&fake).await;

    let sued = db.resolve("Frankfurt (Main) Südbahnhof", None).await.unwrap().unwrap();
    assert_eq!(sued.eva, "8002041");
    let koebenhavn = db.resolve("København H", Some("dk-rejseplanen_000008600626")).await.unwrap().unwrap();
    assert_eq!(koebenhavn.eva, "8600626");

    let log = fake.log();
    let sued_requests: Vec<&str> = log.iter().map(String::as_str).filter(|p| p.contains("Main")).collect();
    assert_eq!(sued_requests, ["/station/Frankfurt(Main)S%C3%BCd"], "stops at the first pattern that finds it");
    assert!(log.contains(&"/station/Frankfurt(M)%20S%C3%BCd".to_owned()));
    assert_eq!(log.last().map(String::as_str), Some("/station/8600626"), "the EVA alone is enough");
}

#[tokio::test]
async fn resolve_caches_hits_and_misses_on_disk() {
    let fake = Arc::new(Fake::default());
    fake.on("/station/Cacheort", 200, &stations_xml(&[("Cacheort", "8000901")]));
    let db = client(&fake).await;
    assert_eq!(db.resolve("Cacheort", None).await.unwrap().map(|s| s.eva), Some("8000901".to_owned()));
    assert!(db.resolve("Nirgendwo Bahnhof", None).await.unwrap().is_none());
    let asked = fake.log().len();
    assert_eq!(asked, 3, "Cacheort, then both patterns of Nirgendwo");
    assert!(db.resolve("Nirgendwo Bahnhof", None).await.unwrap().is_none());
    assert_eq!(fake.log().len(), asked, "known in memory");

    // A new client reads them back from the station cache file.
    let empty = Arc::new(Fake::default());
    let again = client(&empty).await;
    let hit = again.resolve("Cacheort", None).await.unwrap().unwrap();
    assert_eq!((hit.eva.as_str(), hit.name.as_str()), ("8000901", "Cacheort"));
    assert!(again.resolve("Nirgendwo Bahnhof", None).await.unwrap().is_none());
    assert_eq!(empty.log(), Vec::<String>::new(), "nothing asked");
    assert_eq!(again.request_count(), 0);
}

#[tokio::test]
async fn failed_lookups_are_not_cached() {
    let fake = Arc::new(Fake::default());
    fake.on("/station/Pannenstadt", 500, "upstream down")
        .on("/station/Pannenstadt", 500, "upstream down")
        .on("/station/Pannenstadt", 500, "upstream down")
        .on("/station/Pannenstadt", 200, &stations_xml(&[("Pannenstadt", "8000902")]));
    let db = client(&fake).await;
    let error = db.resolve("Pannenstadt", None).await.unwrap_err();
    assert!(format!("{error:#}").contains("500"), "{error:#}");
    assert_eq!(fake.log().len(), MAX_ATTEMPTS as usize, "5xx is retried");
    assert_eq!(db.resolve("Pannenstadt", None).await.unwrap().map(|s| s.eva), Some("8000902".to_owned()));
}

#[tokio::test]
async fn too_many_requests_is_retried() {
    let fake = Arc::new(Fake::default());
    fake.on("/fchg/8000903", 429, "slow down").on("/fchg/8000903", 200, "<timetable/>");
    let db = client(&fake).await;
    assert_eq!(db.get("/fchg/8000903").await.unwrap().as_deref(), Some("<timetable/>"));
    assert_eq!(fake.log().len(), 2);
    assert_eq!(db.request_count(), 2);
}

#[tokio::test]
async fn client_errors_fail_at_once_and_404_means_no_data() {
    let fake = Arc::new(Fake::default());
    fake.on("/plan/8000904/261024/08", 403, "Not registered to plan");
    let db = client(&fake).await;
    let error = db.get("/plan/8000904/261024/08").await.unwrap_err();
    assert!(format!("{error:#}").contains("Not registered to plan"), "{error:#}");
    assert_eq!(db.get("/plan/8000905/261024/08").await.unwrap(), None);
    assert_eq!(fake.log().len(), 2, "neither is retried");

    // Without the right credentials the stand-in refuses; they are sent along.
    let wrong = Db::new(
        &testkit::serve(Router::new().fallback(answer).with_state(fake.clone())).await,
        Credentials { client_id: "id".to_owned(), api_key: "other".to_owned() },
        "umstieg-test",
    )
    .unwrap();
    assert!(wrong.get("/plan/8000905/261024/08").await.is_err());
}

#[tokio::test]
async fn transport_errors_are_retried() {
    let (base, accepted) = testkit::serve_dropping(1, "200 OK", "<timetable/>").await;
    let db = Db::new(&base, Credentials { client_id: "id".to_owned(), api_key: "key".to_owned() }, "t").unwrap();
    assert_eq!(db.get("/fchg/8000906").await.unwrap().as_deref(), Some("<timetable/>"));
    assert_eq!(accepted.load(Ordering::SeqCst), 2);

    let (base, _) = testkit::serve_dropping(usize::MAX, "200 OK", "").await;
    let db = Db::new(&base, Credentials { client_id: "id".to_owned(), api_key: "key".to_owned() }, "t").unwrap();
    assert!(db.get("/fchg/8000906").await.is_err());
    assert_eq!(db.request_count(), MAX_ATTEMPTS as usize);
}

#[tokio::test]
async fn visits_apply_live_changes_to_the_planned_board() {
    let fake = Arc::new(Fake::default());
    fake.on("/plan/8000910/261024/08", 200, PLAN).on("/fchg/8000910", 200, CHANGES);
    let db = client(&fake).await;
    let (visits, coverage) = db.visits("8000910", &[at("06:00")]).await.unwrap();
    let labels = by_label(&visits);
    let mut names: Vec<&str> = labels.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["ECE 151", "ICE 591", "ICE 9999", "RE70 (4557)", "RE70 (4558)", "S1 (38114)", "S3 (38314)"],
        "planned trains plus the added one of this hour"
    );
    assert_eq!(visits.len(), 7, "a change without train data for an unknown train is dropped");

    let ice = labels["ICE 591"];
    let (arrival, departure) = (ice.arrival.as_ref().unwrap(), ice.departure.as_ref().unwrap());
    assert_eq!((arrival.planned, arrival.changed), (Some(at("06:30")), Some(at("06:42"))));
    assert_eq!((departure.planned, departure.changed), (Some(at("06:33")), Some(at("06:45"))));
    assert_eq!((departure.planned_platform.as_deref(), departure.platform()), (Some("5"), Some("6")));
    assert_eq!(arrival.platform(), Some("5"));

    let cancelled = labels["RE70 (4557)"].arrival.as_ref().unwrap();
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.path(), ["Mörfelden", "Groß Gerau-Dornberg", "Riedstadt-Goddelau"]);
    assert_eq!(cancelled.planned_path.len(), 10);
    assert!(!labels["RE70 (4558)"].departure.as_ref().unwrap().cancelled);

    let ece = labels["ECE 151"].departure.as_ref().unwrap();
    assert_eq!((ece.planned, ece.changed, ece.platform()), (Some(at("06:27")), Some(at("06:29")), Some("4")));
    let s3 = labels["S3 (38314)"].departure.as_ref().unwrap();
    assert_eq!((s3.changed, s3.platform()), (None, Some("2")));

    let added = labels["ICE 9999"];
    assert_eq!(added.class, Some('F'));
    assert_eq!(added.departure.as_ref().unwrap().changed, Some(at("06:53")));

    // Coverage: classes with trains on the board, per hour.
    for class in ['F', 'N', 'S'] {
        assert!(coverage.covers(at("06:00"), class), "{class}");
        assert!(coverage.covers(at("06:59"), class), "{class}");
        assert!(!coverage.covers(at("07:00"), class), "{class} in the next hour");
        assert!(!coverage.covers(at("05:59"), class), "{class} in the hour before");
    }
    assert!(!coverage.covers(at("06:30"), 'X'));
}

#[tokio::test]
async fn changes_without_train_data_for_unknown_trains_are_dropped_even_within_the_hours() {
    let fake = Arc::new(Fake::default());
    let changes = r#"<timetable station="Mannheim Hbf" eva="8000244">
<s id="1311239876543210-2610241102-4" eva="8000244"><tl f="F" t="p" o="80" c="ICE" n="9998"/><dp pt="2610241250" pp="2"/></s>
<s id="4242424242-2610241130-5" eva="8000244"><ar pt="2610241240" ct="2610241245"/><dp pt="2610241242" ct="2610241247"/></s>
</timetable>"#;
    fake.on("/fchg/8000913", 200, changes);
    let db = client(&fake).await;
    let (visits, _) = db.visits("8000913", &[at("10:00")]).await.unwrap();
    let labels: Vec<String> = visits.iter().map(Visit::label).collect();
    assert_eq!(labels, ["ICE 9998"], "no number: not a train to ride");
}

#[tokio::test(start_paused = true)]
async fn requests_keep_to_the_quota_per_minute() {
    let db = Db::new("http://unused.test", Credentials { client_id: "id".to_owned(), api_key: "key".to_owned() }, "t")
        .unwrap();
    let start = Instant::now();
    for _ in 0..REQUESTS_PER_MINUTE {
        db.pace().await;
    }
    assert_eq!(start.elapsed(), Duration::ZERO, "a window's quota goes at once");
    db.pace().await;
    let waited = start.elapsed();
    assert!(waited >= QUOTA_WINDOW && waited < QUOTA_WINDOW.saturating_mul(2), "{waited:?}");
    for _ in 1..REQUESTS_PER_MINUTE {
        db.pace().await;
    }
    assert_eq!(start.elapsed(), waited, "the old requests aged out, a new window's quota is free");
    db.pace().await;
    assert!(start.elapsed() >= waited.saturating_add(QUOTA_WINDOW));
    assert_eq!(db.waited(), start.elapsed(), "all the time went into waiting for the quota");
    assert_eq!(db.waiting(), None, "not waiting once a request went out");
}

#[tokio::test]
async fn added_trains_only_within_the_asked_hours() {
    let fake = Arc::new(Fake::default());
    fake.on("/fchg/8000911", 200, CHANGES);
    let db = client(&fake).await;
    let (visits, coverage) = db.visits("8000911", &[at("10:00")]).await.unwrap();
    let labels: Vec<String> = visits.iter().map(Visit::label).collect();
    assert_eq!(labels, ["ICE 9998"], "12:50 CEST is in the 12 o'clock board, 08:50 is not");
    assert!(!coverage.covers(at("10:30"), 'F'), "no board, no coverage: added trains don't count");

    let (visits, _) = db.visits("8000911", &[at("07:00"), at("08:00")]).await.unwrap();
    assert!(visits.is_empty());
}

#[tokio::test]
async fn plans_are_cached_on_disk_changes_fetched_once_per_client() {
    let fake = Arc::new(Fake::default());
    fake.on("/plan/8000912/261024/08", 200, PLAN).on("/fchg/8000912", 200, "<timetable/>");
    let db = client(&fake).await;
    let (first, _) = db.visits("8000912", &[at("06:00"), at("07:00")]).await.unwrap();
    let (second, _) = db.visits("8000912", &[at("06:00")]).await.unwrap();
    assert_eq!((first.len(), second.len()), (6, 6));
    assert_eq!(fake.count("/plan/8000912/261024/08"), 1);
    assert_eq!(fake.count("/plan/8000912/261024/09"), 1, "an empty hour is asked once too");
    assert_eq!(fake.count("/fchg/"), 1);

    // Another client (another run): planned boards come from disk, changes are live.
    let other = Arc::new(Fake::default());
    other.on("/fchg/8000912", 200, CHANGES);
    let again = client(&other).await;
    let (visits, _) = again.visits("8000912", &[at("06:00")]).await.unwrap();
    assert_eq!(other.log(), ["/fchg/8000912"]);
    assert_eq!(visits.len(), 7);
    assert_eq!(by_label(&visits)["ICE 591"].arrival.as_ref().unwrap().changed, Some(at("06:42")));
}

#[tokio::test]
async fn an_unwritable_cache_still_answers() {
    // A file where the cache directory should be: storing answers fails.
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("not-a-dir");
    std::fs::write(&blocked, "").unwrap();
    let fake = Arc::new(Fake::default());
    fake.on("/station/Schreibschutz", 200, &stations_xml(&[("Schreibschutz", "8000913")]))
        .on("/plan/8000913/261024/08", 200, PLAN)
        .on("/fchg/8000913", 200, "<timetable/>");
    let mut db = client(&fake).await;
    db.cache_dir = Some(blocked);
    let station = db.resolve("Schreibschutz", None).await.unwrap().unwrap();
    assert_eq!(station.eva, "8000913");
    let (visits, _) = db.visits("8000913", &[at("06:00")]).await.unwrap();
    assert_eq!(visits.len(), 6);
    db.visits("8000913", &[at("06:00")]).await.unwrap();
    assert_eq!(fake.count("/plan/8000913/261024/08"), 1, "the board is still kept in memory");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_lookups_leave_one_intact_cache_line_each() {
    let fake = Arc::new(Fake::default());
    let names: Vec<String> = (0..40).map(|i| format!("Parallelort {i}")).collect();
    for (i, name) in names.iter().enumerate() {
        let eva = format!("80010{i:02}");
        fake.on(&format!("/station/{}", urlencode(name)), 200, &stations_xml(&[(name.as_str(), eva.as_str())]));
    }
    let clients = [Arc::new(client(&fake).await), Arc::new(client(&fake).await)];
    let lookups: Vec<_> = names
        .iter()
        .zip(clients.iter().cycle())
        .map(|(name, db)| {
            let (db, name) = (db.clone(), name.clone());
            tokio::spawn(async move { db.resolve(&name, None).await })
        })
        .collect();
    for lookup in lookups {
        assert!(lookup.await.unwrap().unwrap().is_some());
    }
    let text = std::fs::read_to_string(crate::cache::root().unwrap().join(STATION_CACHE)).unwrap();
    let cache = parse_station_cache(&text);
    for (i, name) in names.iter().enumerate() {
        assert_eq!(cache.get(name).cloned().flatten().map(|s| s.eva), Some(format!("80010{i:02}")), "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_quota_wait_says_how_long_it_has_left() {
    let db = Arc::new(
        Db::new("http://unused.test", Credentials { client_id: "id".to_owned(), api_key: "key".to_owned() }, "t")
            .unwrap(),
    );
    for _ in 0..REQUESTS_PER_MINUTE {
        db.pace().await;
    }
    assert_eq!(db.waiting(), None);
    let paced = tokio::spawn({
        let db = db.clone();
        async move { db.pace().await }
    });
    // Let it reach the wait, then let a quarter of the window pass.
    tokio::task::yield_now().await;
    tokio::time::advance(QUOTA_WINDOW / 4).await;
    assert_eq!(db.waiting(), QUOTA_WINDOW.checked_sub(QUOTA_WINDOW / 4));
    paced.await.unwrap();
    assert_eq!(db.waiting(), None);
}
