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
use chrono::Utc;
use clap::{ArgAction, CommandFactory, Parser};
use serde::Serialize;
use tokio::sync::Semaphore;

use crate::keys::Keys;
use crate::motis::Motis;
use crate::search::{self, Endpoints, Event, PlotPage, SearchArgs};
use crate::sync::lock;
use crate::{page, plot};

/// Finished searches kept for their result pages.
const KEEP_JOBS: usize = 20;
/// Options not in the form: text output only, or chosen on the plot page itself
/// (metric). The services (`Endpoints`) aren't search options at all.
const NOT_IN_FORM: [&str; 9] =
    ["html", "detail", "per_group", "metric", "help", "version", "from_id", "to_id", "via_id"];
/// Options shown folded under "Advanced".
const ADVANCED: [&str; 5] = ["hubs", "max_nodes", "no_db", "no_trafikverket", "all_stops"];

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
    /// The search, whatever the order of its values: the same link finds the job again.
    key: String,
    started: Instant,
    log: Vec<String>,
    /// How the current stage is getting on, as the last progress said.
    now: Option<String>,
    /// The plot, or why there is none.
    result: Option<Result<plot::Rendered, String>>,
}

impl Job {
    /// A stage or warning goes into the log; progress replaces the last
    /// progress, which the next stage ends.
    fn note(&mut self, line: &str, progress: bool) {
        let line = format!("{:>3}s  {line}", self.started.elapsed().as_secs());
        if progress {
            self.now = Some(line);
        } else {
            self.now = None;
            self.log.push(line);
        }
    }
}

struct App {
    jobs: Mutex<HashMap<u64, Job>>,
    next: AtomicU64,
    permits: Semaphore,
    motis: Motis,
    /// Used by every search, whatever the form says.
    endpoints: Endpoints,
    /// The API keys, read when a search starts.
    keys: fn() -> Result<Keys>,
}

impl App {
    /// `first_job`: the first search's id; see [`first_job`].
    fn new(parallel: usize, endpoints: Endpoints, keys: fn() -> Result<Keys>, first_job: u64) -> Result<Self> {
        Ok(Self {
            jobs: Mutex::new(HashMap::new()),
            next: AtomicU64::new(first_job),
            permits: Semaphore::new(parallel.max(1)),
            motis: Motis::new(&endpoints.api, &endpoints.user_agent())?,
            endpoints,
            keys,
        })
    }
}

/// Job ids continue from the clock, not from 1, so a restarted server doesn't
/// hand out an id whose plot a browser still has cached from before.
fn first_job() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_secs().saturating_mul(1000))
}

pub async fn serve(addr: SocketAddr, parallel: usize, endpoints: Endpoints) -> Result<()> {
    let app = Arc::new(App::new(parallel, endpoints, Keys::load, first_job())?);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("route planner on http://{addr}/");
    axum::serve(listener, router(app)).await?;
    Ok(())
}

/// The server's routes with `keys` instead of the user's, for tests
/// against stand-in services.
#[cfg(test)]
pub fn test_router(parallel: usize, endpoints: Endpoints, keys: fn() -> Result<Keys>) -> Result<Router> {
    Ok(router(Arc::new(App::new(parallel, endpoints, keys, 1)?)))
}

fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(form))
        .route("/stops", get(stops))
        .route("/search", get(search))
        .route("/job/{id}/status", get(status))
        .route("/job/{id}/chains", get(chains))
        .with_state(app)
        // Every request at debug: the progress page polls its status each second.
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

/// How the form shows a field and turns its value back into arguments.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum FieldKind {
    Text,
    Number,
    Flag,
    Choice,
    Time,
    Zone,
    /// A positional station.
    Station,
    /// Several stations, separated by ";".
    Stations,
}

/// One form field, described from the command-line option.
#[derive(Serialize)]
struct Field {
    name: String,
    help: String,
    kind: FieldKind,
    default: String,
    choices: Vec<String>,
    advanced: bool,
}

fn fields() -> Vec<Field> {
    fields_of(&Form::command())
}

/// The form fields for `command`'s options.
fn fields_of(command: &clap::Command) -> Vec<Field> {
    command
        .get_arguments()
        .filter(|a| !NOT_IN_FORM.contains(&a.get_id().as_str()))
        .map(|a| {
            let name = a.get_id().as_str().to_owned();
            let default = a.get_default_values().first().map(|v| v.to_string_lossy().into_owned()).unwrap_or_default();
            let choices: Vec<String> = a.get_possible_values().iter().map(|v| v.get_name().to_owned()).collect();
            let kind = match name.as_str() {
                "from" | "to" => FieldKind::Station,
                "via" => FieldKind::Stations,
                "at" => FieldKind::Time,
                "time_zone" => FieldKind::Zone,
                _ if matches!(a.get_action(), ArgAction::SetTrue) => FieldKind::Flag,
                _ if !choices.is_empty() => FieldKind::Choice,
                _ if default.parse::<f64>().is_ok() => FieldKind::Number,
                _ => FieldKind::Text,
            };
            let choices = if kind == FieldKind::Flag { Vec::new() } else { choices };
            Field {
                help: a.get_help().map(ToString::to_string).unwrap_or_default(),
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
    // After `--`, so that names starting with "-" aren't taken for options.
    let mut stations = Vec::new();
    for f in fields() {
        let option = format!("--{}", f.name.replace('_', "-"));
        match f.kind {
            FieldKind::Station => stations.push(get(&f.name).unwrap_or_default().to_owned()),
            FieldKind::Stations => {
                for v in get(&f.name).unwrap_or_default().split(';').map(str::trim).filter(|v| !v.is_empty()) {
                    argv.push(format!("{option}={v}"));
                }
            }
            FieldKind::Flag => {
                if get(&f.name).is_some() {
                    argv.push(option);
                }
            }
            FieldKind::Text | FieldKind::Number | FieldKind::Choice | FieldKind::Time | FieldKind::Zone => {
                if let Some(v) = get(&f.name) {
                    argv.push(format!("{option}={v}"));
                }
            }
        }
    }
    // Stations as picked in the form: exactly those, not the best match for the name.
    for (name, option) in [("from_id", "--from-id"), ("to_id", "--to-id")] {
        if let Some(id) = get(name) {
            argv.push(format!("{option}={id}"));
        }
    }
    for id in get("via_id").unwrap_or_default().split(';').map(str::trim).filter(|v| !v.is_empty()) {
        argv.push(format!("--via-id={id}"));
    }
    argv.push("--".to_owned());
    argv.extend(stations);
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
    let spec = Spec { fields: fields(), values: params.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(), error };
    Html(page::FORM.render(Some(&spec)).unwrap_or_else(|e| format!("{e:#}")))
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

/// What the progress page is told.
#[derive(Serialize)]
struct Progress {
    /// Where the page polls the search's status.
    status: String,
}

/// The search's own page, so its link is a permalink: its progress while it
/// runs, then its plot. A finished search is kept for a while; after that, or
/// if it failed, the link runs it again (cached answers make that quick). A
/// time relative to today (none for now, "HH:MM") is dated first: opened
/// tomorrow, the link means another search.
async fn search(
    State(app): State<Arc<App>>,
    RawQuery(raw): RawQuery,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let args = match parse(&params) {
        Ok(args) => args,
        Err(error) => return form_page(&params, Some(&error)).into_response(),
    };
    if let Some(link) = search::dated_at(&args, Utc::now()).and_then(|at| dated_link(&params, &at)) {
        return Redirect::to(&link).into_response();
    }
    let key = search_key(&params);
    let mut jobs = lock(&app.jobs);
    let found = jobs.iter().filter(|(_, j)| j.key == key && !matches!(j.result, Some(Err(_)))).map(|(&id, _)| id).max();
    let id = found.unwrap_or_else(|| {
        let id = app.next.fetch_add(1, Ordering::Relaxed);
        let query = raw.unwrap_or_default();
        let job = Job { query, key, started: Instant::now(), log: Vec::new(), now: None, result: None };
        admit(&mut jobs, id, job);
        tokio::spawn(run(app.clone(), id, args));
        id
    });
    let page = if let Some(Ok(plot)) = jobs.get(&id).and_then(|j| j.result.as_ref()) {
        plot.html.clone()
    } else {
        let progress = Progress { status: format!("/job/{id}/status") };
        page::PROGRESS.render(Some(&progress)).unwrap_or_else(|e| format!("{e:#}"))
    };
    drop(jobs);
    // The same link shows progress first, the plot later.
    ([(header::CACHE_CONTROL, "no-cache")], Html(page)).into_response()
}

/// The search's link with `at` in place of whatever time it had.
fn dated_link(params: &[(String, String)], at: &str) -> Option<String> {
    let mut link = reqwest::Url::parse("http://umstieg/search").ok()?;
    link.query_pairs_mut().extend_pairs(params.iter().filter(|(k, _)| k != "at")).append_pair("at", at);
    Some(format!("/search?{}", link.query()?))
}

/// The search's values without blanks, in a fixed order.
fn search_key(params: &[(String, String)]) -> String {
    let mut kept: Vec<(&str, &str)> =
        params.iter().map(|(k, v)| (k.as_str(), v.trim())).filter(|(_, v)| !v.is_empty()).collect();
    kept.sort_unstable();
    format!("{kept:?}")
}

/// Adds a job, forgetting the oldest finished ones beyond `KEEP_JOBS`.
fn admit(jobs: &mut HashMap<u64, Job>, id: u64, job: Job) {
    let mut done: Vec<u64> = jobs.iter().filter(|(_, j)| j.result.is_some()).map(|(&i, _)| i).collect();
    done.sort_unstable();
    for old in done.iter().take(done.len().saturating_sub(KEEP_JOBS)) {
        jobs.remove(old);
    }
    jobs.insert(id, job);
}

#[tracing::instrument(name = "job", skip_all, fields(id = id))]
async fn run(app: Arc<App>, id: u64, args: SearchArgs) {
    let note = |line: &str, progress: bool| {
        if let Some(job) = lock(&app.jobs).get_mut(&id) {
            job.note(line, progress);
        }
    };
    let query = lock(&app.jobs).get(&id).map(|j| j.query.clone()).unwrap_or_default();
    tracing::info!(query, "search asked");
    if app.permits.available_permits() == 0 {
        tracing::info!("waiting for another search to finish");
        note("Waiting for another search to finish", false);
    }
    let Ok(_permit) = app.permits.acquire().await else { return };
    let started = Instant::now();
    let back = Some(format!("/?{query}"));
    let events = |event: Event<'_>| match event {
        Event::Stage(stage) => note(stage, false),
        Event::Warning(warning) => note(&format!("warning: {warning}"), false),
        Event::Progress(line) => note(line, true),
    };
    // The server's services and keys, whatever the URL says.
    let result = match (app.keys)() {
        Ok(keys) => {
            search::run(
                &args,
                &app.endpoints,
                keys,
                Some(PlotPage { back, details_url: Some(format!("/job/{id}/chains")) }),
                &events,
            )
            .await
        }
        Err(e) => Err(e),
    };
    let secs = started.elapsed().as_secs();
    let result = match result {
        Ok(outcome) => {
            tracing::info!(secs, "search done");
            outcome.plot.ok_or(outcome.text)
        }
        Err(e) => {
            tracing::error!(secs, "search failed: {e:#}");
            Err(format!("{e:#}"))
        }
    };
    note(if result.is_ok() { "Done" } else { "Failed" }, false);
    if let Some(job) = lock(&app.jobs).get_mut(&id) {
        job.result = Some(result);
    }
}

async fn status(State(app): State<Arc<App>>, Path(id): Path<u64>) -> Response {
    #[derive(Serialize)]
    struct Status<'a> {
        log: &'a [String],
        /// How the current stage is getting on.
        now: Option<&'a str>,
        done: bool,
        /// Why no plot: an error, or the report saying no connection was found.
        error: Option<&'a str>,
        back: String,
    }
    let jobs = lock(&app.jobs);
    let Some(job) = jobs.get(&id) else { return StatusCode::NOT_FOUND.into_response() };
    let status = Status {
        log: &job.log,
        now: job.now.as_deref(),
        done: job.result.is_some(),
        error: match &job.result {
            Some(Err(e)) => Some(e),
            _ => None,
        },
        back: format!("/?{}", job.query),
    };
    let response = axum::Json(status).into_response();
    drop(jobs);
    response
}

/// Connections the plot page asks about at once, at most.
const MAX_DETAILS: usize = 500;

/// Details of the plot's connections `ids` (comma-separated indexes), in that order.
async fn chains(
    State(app): State<Arc<App>>,
    Path(id): Path<u64>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let ids: Result<Vec<usize>, _> =
        params.get("ids").map(String::as_str).unwrap_or_default().split(',').map(str::parse).collect();
    let Ok(ids) = ids else { return (StatusCode::BAD_REQUEST, "ids: comma-separated numbers").into_response() };
    if ids.len() > MAX_DETAILS {
        return (StatusCode::BAD_REQUEST, format!("at most {MAX_DETAILS} ids at once")).into_response();
    }
    let jobs = lock(&app.jobs);
    let Some(Some(Ok(plot))) = jobs.get(&id).map(|j| j.result.as_ref()) else {
        return (StatusCode::NOT_FOUND, "No plot for this search.").into_response();
    };
    let found: Option<Vec<&plot::Detail>> = ids.iter().map(|&i| plot.details.get(i)).collect();
    let response = found.map_or_else(
        || (StatusCode::NOT_FOUND, "No such connection.").into_response(),
        |details| ([(header::CACHE_CONTROL, "private, max-age=3600")], axum::Json(details)).into_response(),
    );
    drop(jobs);
    response
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
