use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{ADVANCED, App, FieldKind, Form, Job, KEEP_JOBS, MAX_DETAILS, NOT_IN_FORM, admit, fields, parse, router};
use crate::plot::Metric;
use crate::plot::{Detail, Rendered};
use crate::search::Endpoints;
use crate::sync::lock;
use crate::testkit::{self, embedded_json};

fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect()
}

fn job(result: Option<Result<&str, &str>>) -> Job {
    let result = result.map(|r| {
        r.map(|html| Rendered {
            html: html.to_owned(),
            details: vec![
                Detail { route: "08:00 ICE 1 → 09:00 B".to_owned(), fallbacks: vec![(0, "10:00 (+1h)".to_owned())] },
                Detail { route: "08:30 RE 2 → 10:00 B".to_owned(), fallbacks: Vec::new() },
            ],
        })
        .map_err(str::to_owned)
    });
    Job { query: String::new(), key: String::new(), started: Instant::now(), log: Vec::new(), now: None, result }
}

#[test]
fn form_fields_are_the_search_options_minus_text_only_ones() {
    let fields = fields();
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "from",
            "to",
            "at",
            "time_zone",
            "window",
            "max_changes",
            "extra_hours",
            "min_transfer",
            "missed_by",
            "via",
            "hubs",
            "keep_detours",
            "forward_only",
            "max_nodes",
            "no_db",
            "all_stops",
            "planned"
        ]
    );
    let field = |name: &str| fields.iter().find(|f| f.name == name).unwrap();
    let cases = [
        ("from", FieldKind::Station, ""),
        ("to", FieldKind::Station, ""),
        ("via", FieldKind::Stations, ""),
        ("at", FieldKind::Time, ""),
        ("time_zone", FieldKind::Zone, ""),
        ("window", FieldKind::Number, "120"),
        ("extra_hours", FieldKind::Number, "3"),
        ("max_nodes", FieldKind::Number, "20000000"),
        ("forward_only", FieldKind::Flag, ""),
        ("no_db", FieldKind::Flag, ""),
    ];
    for (name, kind, default) in cases {
        assert_eq!((field(name).kind, field(name).default.as_str()), (kind, default), "{name}");
    }
    // Switches are checkboxes, not true/false choices.
    assert!(fields.iter().all(|f| f.choices.is_empty()), "no option with choices is left in the form");
    assert!(field("window").help.contains("window"));
    let advanced: BTreeSet<&str> = fields.iter().filter(|f| f.advanced).map(|f| f.name.as_str()).collect();
    assert_eq!(advanced, BTreeSet::from(ADVANCED));
}

#[test]
fn options_with_choices_or_free_text_get_their_own_kinds() {
    use clap::Arg;
    let command = clap::Command::new("t")
        .arg(Arg::new("sort").long("sort").value_parser(["fastest", "fewest"]).default_value("fastest"))
        .arg(Arg::new("name").long("name").default_value("Fulda"))
        .arg(Arg::new("label").long("label"))
        .arg(Arg::new("count").long("count").default_value("3"));
    let fields = super::fields_of(&command);
    let kinds: Vec<(&str, FieldKind, &str)> =
        fields.iter().map(|f| (f.name.as_str(), f.kind, f.default.as_str())).collect();
    assert_eq!(
        kinds,
        [
            ("sort", FieldKind::Choice, "fastest"),
            ("name", FieldKind::Text, "Fulda"),
            ("label", FieldKind::Text, ""),
            ("count", FieldKind::Number, "3")
        ]
    );
    assert_eq!(fields[0].choices, ["fastest", "fewest"]);
    assert!(fields[1..].iter().all(|f| f.choices.is_empty()));
}

#[test]
fn excluded_and_advanced_names_still_name_options() {
    // A renamed option would silently drop out of these lists (and into the form).
    use clap::CommandFactory;
    let command = Form::command();
    let ids: BTreeSet<&str> = command.get_arguments().map(|a| a.get_id().as_str()).collect();
    // `help` and `version` are clap's own.
    for name in NOT_IN_FORM.iter().chain(&ADVANCED).filter(|n| !["help", "version"].contains(n)) {
        assert!(ids.contains(name), "{name} is no option");
    }
}

#[test]
fn parse_builds_the_search_from_form_values() {
    let args = parse(&params(&[
        ("from", " Mannheim Hbf "),
        ("to", "København H"),
        ("via", "Hamburg Hbf; Fulda ;; "),
        ("at", "2026-10-24T06:30"),
        ("window", "60"),
        ("extra_hours", "1.5"),
        ("forward_only", "on"),
        ("all_stops", ""), // empty: unchecked
        ("hubs", " "),     // blank: the default
    ]))
    .unwrap();
    assert_eq!((args.from.as_str(), args.to.as_str()), ("Mannheim Hbf", "København H"));
    assert_eq!(args.via, ["Hamburg Hbf", "Fulda"]);
    assert_eq!(args.at.as_deref(), Some("2026-10-24T06:30"));
    assert_eq!((args.window, args.extra_hours), (60, 1.5));
    assert!(args.forward_only && !args.all_stops && !args.keep_detours);
    assert_eq!((args.hubs, args.max_changes, args.min_transfer), (6, 15, 5));
}

#[test]
fn picked_stations_carry_their_transitous_ids() {
    let args = parse(&params(&[
        ("from", "Mannheim Hbf"),
        ("from_id", "de-DELFI_de:08222:2417_G"),
        ("to", "København H"),
        ("via", "Hamburg Hbf;Fulda"),
        ("via_id", "hh; fd ;"),
    ]))
    .unwrap();
    assert_eq!((args.from_id.as_deref(), args.to_id.as_deref()), (Some("de-DELFI_de:08222:2417_G"), None));
    assert_eq!(args.via_id, ["hh", "fd"]);
    // Ids look like nothing else in the form, and don't become fields.
    let names: Vec<String> = fields().into_iter().map(|f| f.name).collect();
    assert!(!names.iter().any(|n| n.ends_with("_id")), "{names:?}");
}

#[test]
fn parse_takes_values_literally_even_if_they_look_like_options() {
    let args =
        parse(&params(&[("from", "-Nord"), ("to", "--html=/tmp/x"), ("via", "--detail;-x"), ("at", "--html=/tmp/y")]))
            .unwrap();
    assert_eq!((args.from.as_str(), args.to.as_str()), ("-Nord", "--html=/tmp/x"));
    assert_eq!(args.via, ["--detail", "-x"]);
    assert_eq!(args.at.as_deref(), Some("--html=/tmp/y"));
    assert!(args.html.is_none() && !args.detail);
}

#[test]
fn parse_ignores_options_the_form_does_not_offer() {
    let args = parse(&params(&[
        ("from", "A"),
        ("to", "B"),
        ("html", "/tmp/plot.html"),
        ("detail", "on"),
        ("per_group", "0"),
        ("metric", "ratio"),
        ("api", "http://evil.example"),
        ("contact", "x"),
        ("help", "on"),
        ("bogus", "1"),
    ]))
    .unwrap();
    assert!(args.html.is_none() && !args.detail);
    assert_eq!(args.per_group, 5);
    assert_eq!(args.metric, Metric::RemainingTime);
}

#[test]
fn parse_reports_invalid_values_in_one_paragraph() {
    let cases = [("window", "abc"), ("max_changes", "-1"), ("extra_hours", "lots")];
    for (name, value) in cases {
        let Err(err) = parse(&params(&[("from", "A"), ("to", "B"), (name, value)])) else { panic!("{name} accepted") };
        let option = format!("--{}", name.replace('_', "-"));
        assert!(err.contains(&option) && err.contains(value), "{name}: {err}");
        assert!(!err.starts_with("error:") && !err.contains("\n\n") && !err.contains("--help"), "{name}: {err:?}");
    }
}

#[test]
fn admit_forgets_the_oldest_finished_jobs_beyond_the_limit() {
    let mut jobs = HashMap::new();
    let finished = KEEP_JOBS as u64 + 5;
    for id in 1..=finished {
        admit(&mut jobs, id, job(Some(Ok(""))));
    }
    // A long-running search older than all of them is never dropped.
    jobs.insert(0, job(None));
    admit(&mut jobs, finished + 1, job(None));
    let mut ids: Vec<u64> = jobs.keys().copied().collect();
    ids.sort_unstable();
    let mut want = vec![0];
    want.extend(6..=finished + 1);
    assert_eq!(ids, want);

    // At the limit, nothing goes.
    let mut jobs: HashMap<u64, Job> = (1..=KEEP_JOBS as u64).map(|id| (id, job(Some(Err(""))))).collect();
    admit(&mut jobs, 100, job(None));
    assert_eq!(jobs.len(), KEEP_JOBS + 1);
}

/// A stand-in Transitous: geocode answers from `stops`, counting requests and
/// remembering the texts asked for.
#[derive(Clone, Default)]
struct StandIn {
    asked: Arc<Mutex<Vec<String>>>,
    requests: Arc<AtomicUsize>,
}

async fn stand_in(stops: Value) -> (String, StandIn) {
    let state = StandIn::default();
    let route = get(|State(s): State<StandIn>, Query(q): Query<HashMap<String, String>>| async move {
        s.requests.fetch_add(1, Ordering::SeqCst);
        lock(&s.asked).push(q.get("text").cloned().unwrap_or_default());
        if stops.is_null() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "down".to_owned());
        }
        (StatusCode::OK, stops.to_string())
    });
    let router = axum::Router::new().route("/api/v1/geocode", route).with_state(state.clone());
    (testkit::serve(router).await, state)
}

fn app(api: &str) -> Arc<App> {
    let endpoints = Endpoints {
        efa: String::new(),
        api: api.to_owned(),
        db_api: "http://127.0.0.1:9".to_owned(),
        contact: "test".to_owned(),
    };
    Arc::new(App::new(1, endpoints, || Ok(None), 1).unwrap())
}

async fn get_page(app: &Arc<App>, uri: &str) -> (StatusCode, HashMap<String, String>, String) {
    let response = router(app.clone()).oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or_default().to_owned()))
        .collect();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn form_page_embeds_fields_and_given_values() {
    let app = app("http://127.0.0.1:9");
    let (status, _, html) = get_page(&app, "/?from=Mannheim%20Hbf&window=60&metric=ratio").await;
    assert_eq!(status, StatusCode::OK);
    let spec = embedded_json(&html, "spec");
    assert_eq!(spec["values"], json!({"from": "Mannheim Hbf", "window": "60", "metric": "ratio"}));
    assert_eq!(spec["error"], Value::Null);
    let names: Vec<&str> = spec["fields"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), fields().len());
    assert!(names.contains(&"from") && !names.contains(&"metric"));
}

#[tokio::test]
async fn invalid_search_shows_the_form_again_with_the_error() {
    let (api, transitous) = stand_in(json!([])).await;
    let app = app(&api);
    let (status, _, html) = get_page(&app, "/search?from=A&to=B&window=soon").await;
    assert_eq!(status, StatusCode::OK);
    let spec = embedded_json(&html, "spec");
    assert!(spec["error"].as_str().unwrap().contains("--window"), "{}", spec["error"]);
    assert_eq!(spec["values"]["window"], "soon");
    assert!(lock(&app.jobs).is_empty());
    assert_eq!(transitous.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn out_of_range_values_show_the_form_again_with_the_error() {
    let (api, transitous) = stand_in(json!([])).await;
    let app = app(&api);
    for (name, value, option) in [
        ("window", "99999999999999", "--window"),
        ("extra_hours", "1e300", "--extra-hours"),
        ("min_transfer", "-1", "--min-transfer"),
    ] {
        let (status, _, html) = get_page(&app, &format!("/search?from=A&to=B&{name}={value}")).await;
        assert_eq!(status, StatusCode::OK);
        let spec = embedded_json(&html, "spec");
        assert!(spec["error"].as_str().unwrap().contains(option), "{}", spec["error"]);
        assert_eq!(spec["values"][name], value);
    }
    assert!(lock(&app.jobs).is_empty());
    assert_eq!(transitous.requests.load(Ordering::SeqCst), 0);
}

/// The status of job `id` once it's done. These jobs fail within a second;
/// giving up after 10 s fails a job that never finishes well within the
/// mutation tests' 20 s timeout.
async fn finished(app: &Arc<App>, id: u64) -> Value {
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    loop {
        let (code, _, body) = get_page(app, &format!("/job/{id}/status")).await;
        assert_eq!(code, StatusCode::OK);
        let status: Value = serde_json::from_str(&body).unwrap();
        if status["done"] == true {
            return status;
        }
        assert!(Instant::now() < deadline, "search did not finish: {status}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn log(status: &Value) -> Vec<&str> {
    status["log"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn search_runs_as_a_job_reporting_progress_and_failure() {
    // No stop matches: the search fails right after asking for the stations.
    let (api, transitous) = stand_in(json!([])).await;
    let app = app(&api);
    // The search's link is its page: progress first, told where to poll.
    let (status, headers, page) = get_page(&app, "/search?from=Nowhere&to=B").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL.as_str()], "no-cache");
    assert_eq!(embedded_json(&page, "data"), json!({"status": "/job/1/status"}));

    let status = finished(&app, 1).await;
    // Both stations are looked up at once; whichever fails first is reported.
    let error = status["error"].as_str().unwrap();
    assert!(error.contains("no stop named like \"Nowhere\"") || error.contains("no stop named like \"B\""), "{status}");
    assert_eq!(status["back"], "/?from=Nowhere&to=B");
    let log = log(&status);
    assert!(!log.iter().any(|l| l.contains("Waiting")), "nothing else running: {log:?}");
    assert!(
        log.iter().any(|l| l
            .ends_with("warning: no DB_CLIENT_ID/DB_API_KEY (env or ~/.config/umstieg/db.env); using Transitous only")),
        "{log:?}"
    );
    assert!(log.last().unwrap().ends_with("Failed"), "{log:?}");
    assert!(lock(&transitous.asked).contains(&"Nowhere".to_owned()));

    // A failed search runs again when its link is opened again.
    let (_, _, page) = get_page(&app, "/search?from=Nowhere&to=B").await;
    assert_eq!(embedded_json(&page, "data"), json!({"status": "/job/2/status"}));
    finished(&app, 2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn searches_beyond_the_parallel_limit_wait_their_turn() {
    let (api, _) = stand_in(json!([])).await;
    let app = app(&api);
    let running = app.permits.acquire().await.unwrap();
    let (status, _, _) = get_page(&app, "/search?from=Nowhere&to=B").await;
    assert_eq!(status, StatusCode::OK);
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    let waiting = loop {
        let (_, _, body) = get_page(&app, "/job/1/status").await;
        let status: Value = serde_json::from_str(&body).unwrap();
        if !log(&status).is_empty() {
            break status;
        }
        assert!(Instant::now() < deadline, "no progress noted: {status}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(waiting["done"], false, "{waiting}");
    assert_eq!(log(&waiting).len(), 1, "{waiting}");
    assert!(log(&waiting)[0].ends_with("Waiting for another search to finish"), "{waiting}");

    drop(running);
    let status = finished(&app, 1).await;
    assert!(log(&status).last().unwrap().ends_with("Failed"), "{status}");
}

#[tokio::test]
async fn a_search_link_finds_its_search_again_and_shows_its_plot_once_done() {
    let app = app("http://127.0.0.1:9");
    assert_eq!(get_page(&app, "/job/7/status").await.0, StatusCode::NOT_FOUND);
    let key = |query: &str| {
        super::search_key(&params(&query.split('&').map(|kv| kv.split_once('=').unwrap()).collect::<Vec<_>>()))
    };
    // Order and blank values don't make another search; other values do.
    assert_eq!(key("from=A&to=B&window=60"), key("window=60&to=B&hubs=&from=A"));
    assert_ne!(key("from=A&to=B&window=60"), key("from=A&to=B&window=61"));
    let mut running = job(None);
    running.key = key("from=A&to=B");
    let mut done = job(Some(Ok("<html>plot</html>")));
    done.key = key("from=A&to=C");
    let mut failed = job(Some(Err("No connection found.")));
    failed.key = key("from=A&to=C");
    lock(&app.jobs).insert(1, running);
    lock(&app.jobs).insert(2, done);
    lock(&app.jobs).insert(3, failed);
    let (_, _, page) = get_page(&app, "/search?to=B&from=A").await;
    assert_eq!(embedded_json(&page, "data"), json!({"status": "/job/1/status"}), "still running: its progress");
    let (code, headers, page) = get_page(&app, "/search?from=A&to=C").await;
    assert_eq!((code, page.as_str()), (StatusCode::OK, "<html>plot</html>"), "done: its plot, not the later failure");
    assert_eq!(headers[header::CACHE_CONTROL.as_str()], "no-cache");
    assert_eq!(lock(&app.jobs).len(), 3, "nothing started");

    let (_, _, body) = get_page(&app, "/job/1/status").await;
    let status: Value = serde_json::from_str(&body).unwrap();
    assert_eq!((&status["done"], &status["error"]), (&json!(false), &Value::Null));
    let (_, _, body) = get_page(&app, "/job/3/status").await;
    let status: Value = serde_json::from_str(&body).unwrap();
    assert_eq!((&status["done"], &status["error"]), (&json!(true), &json!("No connection found.")));
}

#[tokio::test]
async fn connection_details_are_handed_out_by_index() {
    let app = app("http://127.0.0.1:9");
    lock(&app.jobs).insert(1, job(None));
    lock(&app.jobs).insert(3, job(Some(Ok("<html>plot</html>"))));
    let (code, headers, body) = get_page(&app, "/job/3/chains?ids=1,0,1").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL.as_str()], "private, max-age=3600");
    let route = |r: &str, fallbacks: Value| json!({"route": r, "fallbacks": fallbacks});
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!([
            route("08:30 RE 2 → 10:00 B", json!([])),
            route("08:00 ICE 1 → 09:00 B", json!([[0, "10:00 (+1h)"]])),
            route("08:30 RE 2 → 10:00 B", json!([]))
        ])
    );
    for (uri, code) in [
        ("/job/3/chains?ids=2", StatusCode::NOT_FOUND),
        ("/job/1/chains?ids=0", StatusCode::NOT_FOUND),
        ("/job/9/chains?ids=0", StatusCode::NOT_FOUND),
        ("/job/3/chains?ids=0,x", StatusCode::BAD_REQUEST),
        ("/job/3/chains", StatusCode::BAD_REQUEST),
    ] {
        assert_eq!(get_page(&app, uri).await.0, code, "{uri}");
    }
    let many = vec!["0"; MAX_DETAILS].join(",");
    assert_eq!(get_page(&app, &format!("/job/3/chains?ids={many}")).await.0, StatusCode::OK);
    let too_many = vec!["0"; MAX_DETAILS + 1].join(",");
    assert_eq!(get_page(&app, &format!("/job/3/chains?ids={too_many}")).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn stops_completes_from_transitous_after_three_characters() {
    let stops = json!([
        {"type": "STOP", "name": "Mannheim Hbf", "id": "a"},
        {"type": "ADDRESS", "name": "Mannheimer Str. 1", "id": "b"},
        {"type": "STOP", "name": "Mannheim Hbf", "id": "c"},
        {"type": "STOP", "name": "Mannheim-Neckarau", "id": "d"},
    ]);
    let (api, transitous) = stand_in(stops).await;
    let app = app(&api);
    let (code, _, body) = get_page(&app, "/stops?q=%20Ma%20").await;
    assert_eq!((code, body.as_str()), (StatusCode::OK, "[]"));
    assert_eq!(transitous.requests.load(Ordering::SeqCst), 0);

    let (code, _, body) = get_page(&app, "/stops?q=%20Man%20").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!([{"name": "Mannheim Hbf", "id": "a"}, {"name": "Mannheim-Neckarau", "id": "d"}])
    );
    assert_eq!(*lock(&transitous.asked), ["Man"]);

    let (api, _) = stand_in(Value::Null).await;
    let (code, _, _) = get_page(&self::app(&api), "/stops?q=Mannheim").await;
    assert_eq!(code, StatusCode::BAD_GATEWAY);
}

#[test]
fn job_ids_continue_from_the_clock_across_restarts() {
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() * 1000;
    let first = super::first_job();
    assert!(first >= before && first < before + 2000, "{first} vs {before}");
}

#[tokio::test]
async fn progress_replaces_progress_until_the_next_stage() {
    let mut j = job(None);
    j.note("Enumerating connections", false);
    j.note("up to 1 changes: 10 states", true);
    j.note("up to 2 changes: 99 states", true);
    assert_eq!((j.log.len(), j.now.as_deref()), (1, Some("  0s  up to 2 changes: 99 states")));
    let app = app("http://127.0.0.1:9");
    lock(&app.jobs).insert(1, j);
    let (_, _, body) = get_page(&app, "/job/1/status").await;
    let status: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status["now"], "  0s  up to 2 changes: 99 states");
    let mut j = lock(&app.jobs).remove(&1).unwrap();
    j.note("Drawing the plot", false);
    assert_eq!((j.log.len(), j.now), (2, None));
}
