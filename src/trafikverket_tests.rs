use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::json;

use super::*;
use crate::testkit::{self, Fault, TV_DATE, TvRun, TvWorld, at, serve_faulty};

fn announcement(ident: &str, sig: &str, activity: &str, hhmm: &str) -> serde_json::Value {
    json!({
        "AdvertisedTrainIdent": ident, "ScheduledDepartureDateTime": TV_DATE,
        "LocationSignature": sig, "ActivityType": activity,
        "AdvertisedTimeAtLocation": at(hhmm).to_rfc3339(),
        "ProductInformation": [{ "Description": "Öresundståg" }],
        "TypeOfTraffic": [{ "Description": "Tåg" }],
    })
}

fn answer_of(announcements: &[serde_json::Value]) -> String {
    json!({ "RESPONSE": { "RESULT": [{ "TrainAnnouncement": announcements }] } }).to_string()
}

fn runs_of(announcements: &[serde_json::Value]) -> Vec<Run> {
    group_runs(parse_announcements(&answer_of(announcements)).unwrap())
}

/// Stops as (signature, arrival, departure) in "HH:MM" ("" none).
fn stops(run: &Run) -> Vec<(String, String, String)> {
    let hhmm = |m: Option<Moment>| m.map(|m| m.planned.format("%H:%M").to_string()).unwrap_or_default();
    run.stops.iter().map(|s| (s.signature.clone(), hhmm(s.arrival), hhmm(s.departure))).collect()
}

fn triple(sig: &str, arr: &str, dep: &str) -> (String, String, String) {
    (sig.to_owned(), arr.to_owned(), dep.to_owned())
}

// ---- answers

#[test]
fn announcements_become_runs_with_one_stop_per_call() {
    // As the API lists them: by time, a departure sometimes before the
    // arrival at the same time (Lme), another train in between.
    let runs = runs_of(&[
        announcement("319", "G", "Avgang", "06:04"),
        announcement("319", "Lme", "Avgang", "07:28"),
        announcement("319", "Lme", "Ankomst", "07:28"),
        announcement("1036", "Hie", "Avgang", "09:18"),
        announcement("319", "Av", "Ankomst", "08:34"),
        announcement("319", "Av", "Avgang", "08:36"),
        announcement("319", "Kac", "Ankomst", "10:02"),
    ]);
    assert_eq!(runs.len(), 2);
    let ident = |r: &Run| r.key.ident.clone();
    let sj = runs.iter().find(|r| ident(r) == "319").unwrap();
    assert_eq!(
        stops(sj),
        [
            triple("G", "", "06:04"),
            triple("Lme", "07:28", "07:28"),
            triple("Av", "08:34", "08:36"),
            triple("Kac", "10:02", "")
        ]
    );
    assert_eq!(sj.key.date, TV_DATE);
    assert_eq!(stops(runs.iter().find(|r| ident(r) == "1036").unwrap()), [triple("Hie", "", "09:18")]);
}

#[test]
fn a_train_calling_twice_at_a_station_has_two_stops_there() {
    // A loop: Kristianstad, Hässleholm, back to Kristianstad.
    let runs = runs_of(&[
        announcement("9", "Cr", "Avgang", "08:00"),
        announcement("9", "Hm", "Ankomst", "08:20"),
        announcement("9", "Hm", "Avgang", "08:25"),
        announcement("9", "Cr", "Ankomst", "08:45"),
        announcement("9", "Cr", "Avgang", "08:50"),
    ]);
    assert_eq!(
        stops(&runs[0]),
        [triple("Cr", "", "08:00"), triple("Hm", "08:20", "08:25"), triple("Cr", "08:45", "08:50")]
    );
}

#[test]
fn only_a_departure_completes_a_stop() {
    // Odd but possible: two arrivals or two departures in a row at one station.
    let runs = runs_of(&[
        announcement("9", "A", "Avgang", "08:00"),
        announcement("9", "A", "Avgang", "08:05"),
        announcement("9", "B", "Ankomst", "08:30"),
        announcement("9", "B", "Ankomst", "08:35"),
        announcement("9", "B", "Avgang", "08:40"),
    ]);
    assert_eq!(
        stops(&runs[0]),
        [triple("A", "", "08:00"), triple("A", "", "08:05"), triple("B", "08:30", ""), triple("B", "08:35", "08:40")]
    );
}

#[test]
fn live_times_cancellations_tracks_and_deletions() {
    let mut late = announcement("1", "A", "Avgang", "08:00");
    late["EstimatedTimeAtLocation"] = json!(at("08:04").to_rfc3339());
    late["TrackAtLocation"] = json!("3a");
    let mut passed = announcement("1", "B", "Ankomst", "09:00");
    passed["EstimatedTimeAtLocation"] = json!(at("09:04").to_rfc3339());
    passed["TimeAtLocation"] = json!(at("09:06").to_rfc3339());
    let mut cancelled = announcement("1", "B", "Avgang", "09:02");
    cancelled["Canceled"] = json!(true);
    let mut deleted = announcement("1", "C", "Ankomst", "10:00");
    deleted["Deleted"] = json!(true);
    let runs = runs_of(&[late, passed, cancelled, deleted]);
    let run = &runs[0];
    let a = run.stops[0].departure.unwrap();
    assert_eq!((a.live, a.cancelled, run.stops[0].track.as_deref()), (Some(at("08:04")), false, Some("3a")));
    let b = &run.stops[1];
    assert_eq!(b.arrival.unwrap().live, Some(at("09:06")), "the actual time wins over the forecast");
    assert_eq!((b.departure.unwrap().live, b.departure.unwrap().cancelled), (None, true));
    assert_eq!(run.stops.len(), 2, "deleted announcements don't count");
}

#[test]
fn products_give_label_number_and_class_and_buses_are_dropped() {
    let with = |ident: &str, product: Option<&str>, traffic: &str| {
        let mut a = announcement(ident, "A", "Avgang", "08:00");
        a["ProductInformation"] = product.map_or(json!([]), |p| json!([{ "Description": p }]));
        a["TypeOfTraffic"] = json!([{ "Description": traffic }]);
        a
    };
    let runs = runs_of(&[
        with("319", Some("SJ Regional"), "Tåg"),
        with("481", Some("SJ Snabbtåg"), "Tåg"),
        with("1036", Some("Öresundståg"), "Tåg"),
        with("2412", Some("SL Pendeltåg"), "Pendeltåg"),
        with("73724", Some("Västtågen"), "Buss"),
        with("8X", None, "Tåg"),
    ]);
    let mut got: Vec<(String, Option<u32>, char)> = runs.iter().map(|r| (r.label(), r.number(), r.class)).collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("8X".to_owned(), None, 'N'),
            ("SJ Regional 319".to_owned(), Some(319), 'F'),
            ("SJ Snabbtåg 481".to_owned(), Some(481), 'F'),
            ("SL Pendeltåg 2412".to_owned(), Some(2412), 'S'),
            ("Öresundståg 1036".to_owned(), Some(1036), 'N'),
        ]
    );
}

#[test]
fn api_errors_fail_with_their_message() {
    let error = r#"{"RESPONSE":{"RESULT":[{"ERROR":{"SOURCE":"Security","MESSAGE":"Invalid authentication"}}]}}"#;
    let e = parse_announcements(error).unwrap_err().to_string();
    assert!(e.contains("Invalid authentication"), "{e}");
    assert!(parse_announcements(r#"{"RESPONSE":{"RESULT":[]}}"#).is_err());
    assert!(parse_announcements("not json").is_err());
}

#[test]
fn station_positions_from_wkt_points() {
    let cases = [
        ("POINT (14.866 56.177)", Some((56.177, 14.866))),
        ("  POINT(11.97 57.71)  ", Some((57.71, 11.97))),
        ("POINT (14.866)", None),
        ("POINT (1 2 3)", None),
        ("POINT (a b)", None),
        ("LINESTRING (1 2, 3 4)", None),
        ("", None),
    ];
    for (wkt, expected) in cases {
        assert_eq!(point(wkt), expected, "{wkt:?}");
    }
}

#[test]
fn attribute_values_are_escaped() {
    assert_eq!(attribute(r#"a"b<c>&d"#), "a&quot;b&lt;c&gt;&amp;d");
    assert_eq!(attribute("Väc"), "Väc");
}

// ---- client

fn world() -> Arc<TvWorld> {
    let run = |ident, product, traffic, stops| TvRun { ident, product, traffic, stops };
    TvWorld::new(
        vec![("G", "Göteborg C", "SE", 57.709, 11.974), ("Dk.kh", "Köpenhamn H", "DK", 55.672, 12.565)],
        vec![
            run("319", "SJ Regional", "Tåg", vec![("G", "", "06:04"), ("Av", "08:34", "08:36"), ("Kac", "10:02", "")]),
            run("1047", "Öresundståg", "Tåg", vec![("G", "", "06:55+3"), ("Lu", "09:36", "09:38")]),
            run("3027", "Västtågen", "Pendeltåg", vec![("G", "", "07:45"), ("Kb", "08:10", "")]),
            run("73724", "Västtågen", "Buss", vec![("G", "", "06:30"), ("X", "07:00", "")]),
            run("1", "Öresundståg", "Tåg", vec![("Lu", "", "06:30"), ("G", "09:00", "")]),
        ],
    )
}

#[tokio::test]
async fn boards_list_each_train_once_without_buses_in_the_asked_span() {
    let world = world();
    let tv = Trafikverket::new(&testkit::serve_trafikverket(&world).await, "key", "umstieg-test").unwrap();
    let seen = tv.board("G", at("06:00"), at("07:45")).await.unwrap();
    let got: Vec<(&str, DateTime<Utc>, char)> =
        seen.iter().map(|s| (s.run.ident.as_str(), s.planned, s.class)).collect();
    // 3027 leaves at 07:45, the end of the span; train 1 arrives at 09:00.
    assert_eq!(got, [("1047", at("06:55"), 'N'), ("319", at("06:04"), 'F')]);
    assert_eq!(world.log(), ["board G"]);
    assert_eq!(tv.request_count(), 1);
}

#[tokio::test]
async fn runs_come_whole_in_batches() {
    let world = world();
    let tv = Trafikverket::new(&testkit::serve_trafikverket(&world).await, "key", "umstieg-test").unwrap();
    let key = |ident: &str| RunKey { ident: ident.to_owned(), date: TV_DATE.to_owned() };
    // Two real runs among unknown ones, more than fit into one request.
    let mut keys: Vec<RunKey> = (0..RUNS_PER_REQUEST).map(|n| key(&format!("9{n:04}"))).collect();
    keys.extend([key("319"), key("1047")]);
    let runs = tv.runs(&keys).await.unwrap();
    let mut got: Vec<(String, usize)> = runs.iter().map(|r| (r.key.ident.clone(), r.stops.len())).collect();
    got.sort();
    assert_eq!(got, [("1047".to_owned(), 2), ("319".to_owned(), 3)]);
    assert_eq!(tv.request_count(), 2);
    let late = runs.iter().find(|r| r.key.ident == "1047").unwrap().stops[0].departure.unwrap();
    assert_eq!((late.planned, late.live), (at("06:55"), Some(at("06:58"))));
    assert_eq!(tv.runs(&[]).await.unwrap(), Vec::<Run>::new());
    assert_eq!(tv.request_count(), 2, "nothing asked for, no request");
}

#[tokio::test]
async fn stations_are_fetched_once_and_cached_on_disk() {
    let world = world();
    let url = testkit::serve_trafikverket(&world).await;
    let tv = Trafikverket::new(&url, "key", "umstieg-test").unwrap();
    let list = tv.stations().await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(
        list[0],
        Station {
            signature: "G".to_owned(),
            name: "Göteborg C".to_owned(),
            country: "SE".to_owned(),
            position: Some((57.709, 11.974)),
        }
    );
    tv.stations().await.unwrap();
    // A new client (the next search) reads the cache.
    let next = Trafikverket::new(&url, "key", "umstieg-test").unwrap();
    assert_eq!(next.stations().await.unwrap().len(), 2);
    assert_eq!(world.log(), ["stations"]);
}

#[tokio::test]
async fn each_api_has_its_own_cached_stations() {
    let world = world();
    let tv = Trafikverket::new(&testkit::serve_trafikverket(&world).await, "key", "umstieg-test").unwrap();
    assert_eq!(tv.stations().await.unwrap().len(), 2);
    let other = TvWorld::new(vec![("Cst", "Stockholm C", "SE", 59.330, 18.058)], Vec::new());
    let elsewhere = Trafikverket::new(&testkit::serve_trafikverket(&other).await, "key", "umstieg-test").unwrap();
    assert_eq!(elsewhere.stations().await.unwrap()[0].signature, "Cst");
    assert_eq!(other.log(), ["stations"]);
}

#[tokio::test]
async fn a_wrong_key_fails() {
    let world = world();
    let tv = Trafikverket::new(&testkit::serve_trafikverket(&world).await, "wrong", "umstieg-test").unwrap();
    let e = tv.board("G", at("06:00"), at("08:00")).await.unwrap_err().to_string();
    assert!(e.contains("Invalid authentication"), "{e}");
}

#[tokio::test]
async fn dropped_connections_are_retried_then_given_up() {
    let body = r#"{"RESPONSE":{"RESULT":[{"TrainAnnouncement":[]}]}}"#;
    let (url, accepted) = serve_faulty(Fault::Drop, 2, "200 OK", body).await;
    let tv = Trafikverket::new(&url, "key", "umstieg-test").unwrap();
    assert_eq!(tv.board("G", at("06:00"), at("08:00")).await.unwrap(), Vec::<Seen>::new());
    assert_eq!(accepted.load(Ordering::SeqCst), 3);

    let (url, _) = serve_faulty(Fault::Drop, 5, "200 OK", body).await;
    let tv = Trafikverket::new(&url, "key", "umstieg-test").unwrap();
    assert!(tv.board("G", at("06:00"), at("08:00")).await.is_err());
    assert_eq!(tv.request_count(), MAX_ATTEMPTS as usize);
}

#[tokio::test]
async fn busy_servers_are_retried() {
    use axum::http::StatusCode;
    let answers = Arc::new(AtomicUsize::new(0));
    let count = answers.clone();
    let router = axum::Router::new().fallback(move || {
        let n = count.fetch_add(1, Ordering::SeqCst);
        async move {
            let empty = r#"{"RESPONSE":{"RESULT":[{"TrainAnnouncement":[]}]}}"#.to_owned();
            match n {
                0 => (StatusCode::SERVICE_UNAVAILABLE, String::new()),
                1 => (StatusCode::TOO_MANY_REQUESTS, String::new()),
                _ => (StatusCode::OK, empty),
            }
        }
    });
    let tv = Trafikverket::new(&testkit::serve(router).await, "key", "umstieg-test").unwrap();
    assert_eq!(tv.board("G", at("06:00"), at("08:00")).await.unwrap(), Vec::<Seen>::new());
    assert_eq!(answers.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn http_errors_fail_with_their_status() {
    let (url, _) = serve_faulty(Fault::Drop, 0, "400 Bad Request", "bad query").await;
    let tv = Trafikverket::new(&url, "key", "umstieg-test").unwrap();
    let e = tv.board("G", at("06:00"), at("08:00")).await.unwrap_err().to_string();
    assert!(e.contains("400") && e.contains("bad query"), "{e}");
}
