//! `umstieg serve`: the route planner in the browser. A form generated from
//! the command-line options, a progress page while the search runs, and the
//! plot as result, linking back to the form with the same settings.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use axum::Router;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use clap::{ArgAction, CommandFactory, Parser};
use serde::Serialize;
use tokio::sync::Semaphore;

use crate::motis::Motis;
use crate::search::{self, Endpoints, Event, SearchArgs};

const FORM: &str = include_str!("form.html");
const PROGRESS: &str = include_str!("progress.html");
/// Finished searches kept for their result pages.
const KEEP_JOBS: usize = 20;
/// Options not in the form: text output only, or chosen on the plot page itself
/// (metric). The services (`Endpoints`) aren't search options at all.
const NOT_IN_FORM: [&str; 6] = ["html", "detail", "per_group", "metric", "help", "version"];
/// Options shown folded under "Advanced".
const ADVANCED: [&str; 4] = ["hubs", "max_nodes", "no_db", "all_stops"];

/// The search options as a command line, for validation and defaults.
#[derive(Parser)]
#[command(name = "umstieg")]
struct Form {
    #[command(flatten)]
    args: SearchArgs,
}

struct Job {
    /// The form's query string, for the link back to it.
    query: String,
    started: Instant,
    log: Vec<String>,
    result: Option<Result<String, String>>,
}

struct App {
    jobs: Mutex<HashMap<u64, Job>>,
    next: AtomicU64,
    permits: Semaphore,
    motis: Motis,
    /// Used by every search, whatever the form says.
    endpoints: Endpoints,
}

pub async fn serve(addr: SocketAddr, parallel: usize, endpoints: Endpoints) -> Result<()> {
    let app = Arc::new(App {
        jobs: Mutex::new(HashMap::new()),
        next: AtomicU64::new(1),
        permits: Semaphore::new(parallel.max(1)),
        motis: Motis::new(&endpoints.api, &endpoints.user_agent())?,
        endpoints,
    });
    let router = Router::new()
        .route("/", get(form))
        .route("/stops", get(stops))
        .route("/search", get(start))
        .route("/job/{id}", get(progress))
        .route("/job/{id}/status", get(status))
        .route("/job/{id}/plot", get(plot))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("umstieg: route planner on http://{addr}/");
    axum::serve(listener, router).await?;
    Ok(())
}

/// One form field, described from the command-line option.
#[derive(Serialize)]
struct Field {
    name: String,
    help: String,
    /// "text", "number", "flag", "choice", "time", "station", "stations".
    kind: &'static str,
    default: String,
    choices: Vec<String>,
    advanced: bool,
}

fn fields() -> Vec<Field> {
    let command = Form::command();
    command
        .get_arguments()
        .filter(|a| !NOT_IN_FORM.contains(&a.get_id().as_str()))
        .map(|a| {
            let name = a.get_id().as_str().to_owned();
            let default = a.get_default_values().first().map(|v| v.to_string_lossy().into_owned()).unwrap_or_default();
            let choices: Vec<String> = a.get_possible_values().iter().map(|v| v.get_name().to_owned()).collect();
            let kind = match name.as_str() {
                "from" | "to" => "station",
                "via" => "stations",
                "at" => "time",
                _ if matches!(a.get_action(), ArgAction::SetTrue) => "flag",
                _ if !choices.is_empty() => "choice",
                _ if default.parse::<f64>().is_ok() => "number",
                _ => "text",
            };
            let choices = if kind == "flag" { Vec::new() } else { choices };
            Field {
                help: a.get_help().map(|h| h.to_string()).unwrap_or_default(),
                advanced: ADVANCED.contains(&name.as_str()),
                name,
                kind,
                default,
                choices,
            }
        })
        .collect()
}

/// The form's values as a command line: validated and completed by clap.
fn parse(params: &[(String, String)]) -> Result<SearchArgs, String> {
    let get = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.trim()).filter(|v| !v.is_empty());
    let mut argv = vec!["umstieg".to_owned()];
    for f in fields() {
        match f.kind {
            "station" => argv.push(get(&f.name).unwrap_or_default().to_owned()),
            "stations" => {
                for v in get(&f.name).unwrap_or_default().split(';').map(str::trim).filter(|v| !v.is_empty()) {
                    argv.extend([format!("--{}", f.name.replace('_', "-")), v.to_owned()]);
                }
            }
            "flag" => {
                if get(&f.name).is_some() {
                    argv.push(format!("--{}", f.name.replace('_', "-")));
                }
            }
            _ => {
                if let Some(v) = get(&f.name) {
                    argv.push(format!("--{}={v}", f.name.replace('_', "-")));
                }
            }
        }
    }
    Form::try_parse_from(argv).map(|f| f.args).map_err(|e| {
        // The first paragraph; the rest points to `--help`.
        e.render().to_string().split("\n\n").next().unwrap_or_default().trim_start_matches("error: ").to_owned()
    })
}

fn form_page(params: &[(String, String)], error: Option<&str>) -> Html<String> {
    #[derive(Serialize)]
    struct Spec<'a> {
        fields: Vec<Field>,
        values: HashMap<&'a str, &'a str>,
        error: Option<&'a str>,
    }
    let spec = Spec {
        fields: fields(),
        values: params.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(),
        error,
    };
    let json = serde_json::to_string(&spec).unwrap_or_default().replace("</", "<\\/");
    Html(FORM.replace("/*__SPEC__*/null", &json))
}

async fn form(Query(params): Query<Vec<(String, String)>>) -> Html<String> {
    form_page(&params, None)
}

async fn stops(State(app): State<Arc<App>>, Query(params): Query<HashMap<String, String>>) -> Response {
    let q = params.get("q").map(|q| q.trim()).unwrap_or_default();
    if q.chars().count() < 3 {
        return axum::Json(Vec::<String>::new()).into_response();
    }
    match app.motis.suggest(q).await {
        Ok(names) => axum::Json(names).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn start(
    State(app): State<Arc<App>>,
    RawQuery(raw): RawQuery,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let args = match parse(&params) {
        Ok(args) => args,
        Err(error) => return form_page(&params, Some(&error)).into_response(),
    };
    let id = app.next.fetch_add(1, Ordering::Relaxed);
    {
        let mut jobs = app.jobs.lock().expect("jobs poisoned");
        // Forget the oldest finished searches.
        let mut done: Vec<u64> = jobs.iter().filter(|(_, j)| j.result.is_some()).map(|(&i, _)| i).collect();
        done.sort_unstable();
        for old in done.iter().take(done.len().saturating_sub(KEEP_JOBS)) {
            jobs.remove(old);
        }
        jobs.insert(id, Job { query: raw.unwrap_or_default(), started: Instant::now(), log: Vec::new(), result: None });
    }
    tokio::spawn(run(app, id, args));
    Redirect::to(&format!("/job/{id}")).into_response()
}

async fn run(app: Arc<App>, id: u64, args: SearchArgs) {
    let note = |line: String| {
        if let Some(job) = app.jobs.lock().expect("jobs poisoned").get_mut(&id) {
            let secs = job.started.elapsed().as_secs();
            job.log.push(format!("{:>3}s  {line}", secs));
        }
    };
    if app.permits.available_permits() == 0 {
        note("Waiting for another search to finish".to_owned());
    }
    let Ok(_permit) = app.permits.acquire().await else { return };
    let back = app.jobs.lock().expect("jobs poisoned").get(&id).map(|j| format!("/?{}", j.query));
    let events = |event: Event<'_>| match event {
        Event::Stage(stage) => note(stage.to_owned()),
        Event::Warning(warning) => note(format!("warning: {warning}")),
    };
    // The server's services, whatever the URL says.
    let result = search::run(&args, &app.endpoints, Some(back), &events).await;
    let result = match result {
        Ok(outcome) => outcome.html.ok_or(outcome.text),
        Err(e) => Err(format!("{e:#}")),
    };
    note(if result.is_ok() { "Done".to_owned() } else { "Failed".to_owned() });
    if let Some(job) = app.jobs.lock().expect("jobs poisoned").get_mut(&id) {
        job.result = Some(result);
    }
}

async fn progress(State(app): State<Arc<App>>, Path(id): Path<u64>) -> Response {
    if !app.jobs.lock().expect("jobs poisoned").contains_key(&id) {
        return (StatusCode::NOT_FOUND, "No such search (finished searches are kept for a while only).").into_response();
    }
    Html(PROGRESS.replace("__ID__", &id.to_string())).into_response()
}

async fn status(State(app): State<Arc<App>>, Path(id): Path<u64>) -> Response {
    #[derive(Serialize)]
    struct Status<'a> {
        log: &'a [String],
        done: bool,
        /// Why no plot: an error, or the report saying no connection was found.
        error: Option<&'a str>,
        back: String,
    }
    let jobs = app.jobs.lock().expect("jobs poisoned");
    let Some(job) = jobs.get(&id) else { return StatusCode::NOT_FOUND.into_response() };
    let status = Status {
        log: &job.log,
        done: job.result.is_some(),
        error: match &job.result {
            Some(Err(e)) => Some(e),
            _ => None,
        },
        back: format!("/?{}", job.query),
    };
    axum::Json(status).into_response()
}

async fn plot(State(app): State<Arc<App>>, Path(id): Path<u64>) -> Response {
    let jobs = app.jobs.lock().expect("jobs poisoned");
    match jobs.get(&id).and_then(|j| j.result.as_ref()) {
        Some(Ok(html)) => ([(header::CACHE_CONTROL, "private, max-age=3600")], Html(html.clone())).into_response(),
        _ => (StatusCode::NOT_FOUND, "No plot for this search.").into_response(),
    }
}
