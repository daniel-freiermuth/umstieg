//! One search, from stations and limits to the text report and the plot:
//! shared by the command line and `umstieg serve`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use futures::future::{join_all, try_join_all};
use tokio::sync::mpsc::UnboundedSender;

use crate::enumerate::{self, Chain, Limits, Network, REPORT_EVERY, Step};
use crate::fallback::{self, Fallbacks};
use crate::keys::Keys;
use crate::motis::{Motis, Stop};
use crate::sj::{self, Sj};
use crate::text::{buffer, clock, duration, header, route, timed, window};
use crate::trafikverket::{self, Trafikverket};
use crate::{checked, complete, db, efa, plot, rough, timetable, usable};

/// Upper limit of the options in minutes (`--window`, `--min-transfer`,
/// `--missed-by`): a week, far beyond any search the timetable covers.
const MAX_MINUTES: i64 = 7 * 24 * 60;
/// Upper limit of `--extra-hours`, a week too.
const MAX_EXTRA_HOURS: f64 = 7.0 * 24.0;
/// Upper limit of `--max-changes`: more than any journey has.
const MAX_CHANGES: usize = 100;

/// `--extra-hours`: a number of hours from 0 to a week.
fn extra_hours(s: &str) -> Result<f64, String> {
    let hours: f64 = s.parse().map_err(|e| format!("{e}"))?;
    if (0.0..=MAX_EXTRA_HOURS).contains(&hours) {
        Ok(hours)
    } else {
        Err(format!("{hours} is not in 0..={MAX_EXTRA_HOURS}"))
    }
}

/// `--max-changes`: 0 to `MAX_CHANGES`.
fn max_changes(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|e| format!("{e}"))?;
    if n <= MAX_CHANGES { Ok(n) } else { Err(format!("{n} is not in 0..={MAX_CHANGES}")) }
}

/// `--time-zone`: an IANA name such as "Europe/Berlin".
fn time_zone(s: &str) -> Result<Tz, String> {
    s.parse().map_err(|_| format!("{s:?} is not a time zone name such as Europe/Berlin"))
}

/// This machine's time zone: TZ ("Europe/Berlin", ":Europe/Berlin" or a
/// zoneinfo path) if set, else the system's.
fn machine_zone() -> Result<Tz> {
    let name = match std::env::var("TZ") {
        Ok(tz) if !tz.is_empty() => {
            let tz = tz.trim_start_matches(':');
            tz.split_once("zoneinfo/").map_or(tz, |(_, name)| name).to_owned()
        }
        _ => iana_time_zone::get_timezone().context("cannot tell this machine's time zone; give --time-zone")?,
    };
    time_zone(&name).map_err(|e| anyhow!("this machine's time zone: {e}; give --time-zone"))
}

/// Stations, time and every limit of a search. The web form is generated from
/// these arguments, so options added here show up there too.
#[derive(clap::Args, Clone)]
#[expect(clippy::struct_excessive_bools, reason = "command-line switches, one field each")]
pub struct SearchArgs {
    /// Origin station: its name; the best-matching stop is taken.
    pub from: String,
    /// Destination station: its name; the best-matching stop is taken.
    pub to: String,
    /// Earliest departure in --time-zone: "HH:MM" (today) or "YYYY-MM-DD HH:MM". Default: now.
    #[arg(long)]
    pub at: Option<String>,
    /// Time zone of --at and of every time shown, e.g. "Europe/Berlin". Default:
    /// this machine's (TZ, else the system's).
    #[arg(long, value_parser = time_zone)]
    pub time_zone: Option<Tz>,
    /// Departure window in minutes.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(i64).range(0..=MAX_MINUTES))]
    pub window: i64,
    /// Maximum number of changes.
    #[arg(long, default_value_t = 15, value_parser = max_changes)]
    pub max_changes: usize,
    /// Show connections taking at most this many hours longer than the fastest.
    #[arg(long, default_value_t = 3.0, value_parser = extra_hours)]
    pub extra_hours: f64,
    /// Changes shorter than this many minutes are flagged with ⚠.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(i64).range(0..=MAX_MINUTES))]
    pub min_transfer: i64,
    /// Also list changes that live data says are missed by up to this many
    /// minutes (marked ✗): delays can still shift either way. 0 = only feasible ones.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(i64).range(0..=MAX_MINUTES))]
    pub missed_by: i64,
    /// List every connection within the limits, not only the best ones (none
    /// leaving later, arriving earlier, with fewer changes and a bigger
    /// buffer at once). Slow to draw for a busy corridor.
    #[arg(long)]
    pub all: bool,
    /// Connections listed per (first train, last train) group; 0 = all.
    #[arg(long, default_value_t = 5)]
    pub per_group: usize,
    /// Show every connection with platforms and what happens if a change is missed.
    #[arg(long)]
    pub detail: bool,
    /// Write an interactive time–distance plot of all connections to this HTML file.
    #[arg(long)]
    pub html: Option<PathBuf>,
    /// Height of a station in the plot (first shown; the page can switch).
    #[arg(long, value_enum, default_value_t = plot::Metric::RemainingTime)]
    pub metric: plot::Metric,
    /// Station to route the rough search through; repeatable.
    #[arg(long)]
    pub via: Vec<String>,
    /// Transitous stop id of FROM: the station exactly, as the web form picks
    /// it, instead of the best match for the name.
    #[arg(long, hide = true)]
    pub from_id: Option<String>,
    /// Transitous stop id of TO.
    #[arg(long, hide = true)]
    pub to_id: Option<String>,
    /// Transitous stop ids of the --via stations, in the same order.
    #[arg(long, hide = true)]
    pub via_id: Vec<String>,
    /// Transfer stations of the planner's answers used to widen the rough search.
    #[arg(long, default_value_t = 6)]
    pub hubs: usize,
    /// Also list connections that change to a train they could have boarded at an earlier stop.
    #[arg(long)]
    pub keep_detours: bool,
    /// Only rides that reduce the best-case remaining travel time (no riding away
    /// from the destination): fewer, more direct connections, much faster search.
    #[arg(long)]
    pub forward_only: bool,
    /// Search effort cap (explored states; ~1 s per million).
    #[arg(long, default_value_t = 20_000_000)]
    pub max_nodes: usize,
    /// Don't use DB's station boards even if credentials are configured.
    #[arg(long)]
    pub no_db: bool,
    /// Don't use Trafikverket's train announcements (Sweden) even if a key is configured.
    #[arg(long)]
    pub no_trafikverket: bool,
    /// Fetch DB boards for every stop of the rough routes' trains, not only where
    /// they start or end: more change points, ~3× the DB requests on a cold cache.
    #[arg(long)]
    pub all_stops: bool,
    /// Ignore delays: planned times only. DB's latest schedule (cancellations,
    /// diversions, extra trains) still applies.
    #[arg(long)]
    pub planned: bool,
}

/// Services asked and how we identify to them: set by whoever runs umstieg
/// (the command line, or `umstieg serve` for all its searches), not per search
/// in the web form. Kept out of `SearchArgs`: flattened into it, clap emptied
/// its argument group and `Option<SearchArgs>` came out `None` (clap 4.6).
#[derive(clap::Args, Clone)]
pub struct Endpoints {
    /// EFA journey planner asked for local legs between German stops (empty: none).
    /// It knows trams and buses Transitous can miss.
    #[arg(long, default_value = "https://www.vrn.de/mngvrn")]
    pub efa: String,
    /// MOTIS API base URL.
    #[arg(long, default_value = "https://api.transitous.org")]
    pub api: String,
    /// DB Timetables API base URL.
    #[arg(long, default_value = db::API)]
    pub db_api: String,
    /// Trafikverket open API URL.
    #[arg(long, default_value = trafikverket::API)]
    pub trafikverket_api: String,
    /// SJ's booking API, asked for connections through Sweden (empty: not asked).
    #[arg(long, default_value = sj::API)]
    pub sj: String,
    /// Contact sent in the User-Agent, as the Transitous usage policy requires.
    #[arg(long, env = "UMSTIEG_CONTACT", default_value = "https://github.com/daniel-freiermuth")]
    pub contact: String,
}

impl Endpoints {
    pub fn user_agent(&self) -> String {
        format!("umstieg/{} ({})", env!("CARGO_PKG_VERSION"), self.contact)
    }
}

/// Progress of a running search.
pub enum Event<'a> {
    Stage(&'a str),
    Warning(&'a str),
    /// How the current stage is getting on, about every `REPORT_EVERY`; each
    /// replaces the one before.
    Progress(&'a str),
}

pub struct Outcome {
    /// The text report, as the command line prints it.
    pub text: String,
    /// The plot page and its connections' details, if asked for.
    pub plot: Option<plot::Rendered>,
}

/// Asks `run` for the plot page.
pub struct PlotPage {
    /// Link back to the search form that started the search.
    pub back: Option<String>,
    /// Where the page fetches its connections' details (`umstieg serve`);
    /// `None` puts them into the page (a file).
    pub details_url: Option<String>,
}

/// Runs a search; with `plot`, also renders the plot page. DB and Trafikverket
/// are asked if `keys` has theirs (and `--no-db`/`--no-trafikverket` don't
/// say otherwise).
#[tracing::instrument(name = "search", skip_all, fields(from = %args.from, to = %args.to))]
pub async fn run(
    args: &SearchArgs,
    endpoints: &Endpoints,
    keys: Keys,
    plot: Option<PlotPage>,
    events: &(dyn Fn(Event<'_>) + Sync),
) -> Result<Outcome> {
    // Warnings also go to the operator's log.
    let events = &|event: Event<'_>| {
        if let Event::Warning(warning) = event {
            tracing::warn!("{warning}");
        }
        events(event);
    };
    let user_agent = endpoints.user_agent();
    let motis = Motis::new(&endpoints.api, &user_agent)?;
    let feeds = Feeds::new(args, endpoints, keys, &user_agent, events)?;
    let (efa, db, tv) = (feeds.efa.as_ref(), feeds.db.as_ref(), feeds.tv.as_ref());

    let zone = match args.time_zone {
        Some(zone) => zone,
        None => machine_zone()?,
    };
    let time = parse_time_in(args.at.as_deref(), zone, Utc::now())?;
    // Options are range-checked by clap; checked again here for searches set up
    // in code, so that none of them can make the time arithmetic overflow.
    let window = checked::minutes(args.window, "--window")?;
    let depart_until = checked::later(time, window, "--window")?;
    let min_transfer = checked::minutes(args.min_transfer, "--min-transfer")?;
    let missed_by = checked::minutes(args.missed_by.max(0), "--missed-by")?;
    let extra_time = extra_time(args.extra_hours)?;
    events(Event::Stage("Finding stations"));
    let (origin, destination, vias) = stations(&motis, args).await?;
    let asked = || feeds.so_far(&motis);

    events(Event::Stage("Asking Transitous and EFA for rough routes"));
    let query = rough::Query { origin, destination, time, window, vias, auto_hubs: args.hubs };
    let mut rough = ticking(rough::collect(&motis, efa, &query), events, asked).await?;
    let with_sj = with_sj(&feeds, &query, &mut rough, events, asked).await;
    events(Event::Stage("Building the timetable from station boards (DB, Trafikverket)"));
    let boards =
        timetable::build(&motis, db, tv, &rough.itineraries, &query.origin, &query.destination, args.all_stops);
    let mut built = ticking(boards, events, asked).await;
    if args.planned {
        built.timetable.ignore_delays();
    }
    for warning in rough.warnings.iter().chain(&built.warnings) {
        events(Event::Warning(warning));
    }

    events(Event::Stage("Enumerating connections"));
    let limits = Limits {
        origin: built.timetable.origin,
        depart_from: time,
        depart_until,
        max_changes: args.max_changes,
        extra_time,
        min_transfer,
        missed_by,
        keep_detours: args.keep_detours,
        forward_only: args.forward_only,
        max_nodes: args.max_nodes,
    };
    let (mut built, mut enumeration, limits) = blocking(events, move |updates| {
        let report = |p: enumerate::Progress| {
            let found = format!("{} connections found", p.found);
            let line = format!("up to {} changes: {} states explored, {found}", p.changes, p.nodes);
            updates.send(Update::Progress(line)).ok();
        };
        let enumeration = enumerate::enumerate_reporting(&Network::new(&built.timetable)?, &limits, &report)?;
        Ok((built, enumeration, limits))
    })
    .await?;
    ticking(complete_stops(&motis, &mut built.timetable, &mut enumeration.chains, events), events, asked).await;
    let requests = feeds.summary(&motis);
    // What the search cost the services, for the operator (Transitous asks to keep the load light).
    tracing::info!(
        transitous = motis.request_count(),
        efa = efa.map(efa::Efa::request_count),
        db = db.map(db::Db::request_count),
        trafikverket = tv.map(Trafikverket::request_count),
        sj = feeds.sj.as_ref().map(Sj::request_count),
        db_quota_wait = ?db.map(db::Db::waited),
        "requests sent"
    );
    let report = Report {
        args: args.clone(),
        built,
        enumeration,
        limits,
        hubs: rough.hubs,
        planner_first: rough.first,
        requests,
        with_db: db.is_some(),
        with_sj,
        zone,
        time,
        depart_until,
        min_transfer,
        plot,
    };
    blocking(events, move |updates| report.write(updates)).await
}

/// Often there is a connection, only leaving after the window (the last bus
/// has gone): when the planner's first one leaves and arrives, if any.
fn later(planner_first: Option<(DateTime<Utc>, DateTime<Utc>)>, zone: Tz) -> String {
    let at = |t: DateTime<Utc>| format!("{}, {}", t.with_timezone(&zone).format("%a %-d %b"), clock(t, zone));
    planner_first.map_or_else(String::new, |(leaves, arrives)| {
        format!("\nTransitous's first connection leaves {} and arrives {}.", at(leaves), at(arrives))
    })
}

/// Origin, destination and vias: as picked in the form, or the best match for the names.
async fn stations(motis: &Motis, args: &SearchArgs) -> Result<(Stop, Stop, Vec<Stop>)> {
    check_via_ids(args)?;
    let (origin, destination) = tokio::try_join!(
        station(motis, &args.from, args.from_id.as_deref()),
        station(motis, &args.to, args.to_id.as_deref())
    )?;
    let via_ids = args.via_id.iter().map(|id| Some(id.as_str())).chain(std::iter::repeat(None));
    let vias = try_join_all(args.via.iter().zip(via_ids).map(|(name, id)| station(motis, name, id))).await?;
    Ok((origin, destination, vias))
}

/// Via ids, if given, pair up with the vias.
fn check_via_ids(args: &SearchArgs) -> Result<()> {
    if !args.via_id.is_empty() && args.via_id.len() != args.via.len() {
        bail!("--via-id: one per --via ({} for {})", args.via_id.len(), args.via.len());
    }
    Ok(())
}

/// The station picked in the form (`id`), or the best match for `name`.
async fn station(motis: &Motis, name: &str, id: Option<&str>) -> Result<Stop> {
    match id {
        Some(id) => motis.stop_by_id(name, id).await,
        None => motis.stop(name).await,
    }
}

/// Fills in the stops of trains in `chains` known only from DB boards, from
/// Transitous (see `complete`). Failures only cost those stops.
async fn complete_stops(
    motis: &Motis,
    tt: &mut timetable::Timetable,
    chains: &mut [Chain],
    events: &(dyn Fn(Event<'_>) + Sync),
) {
    let wanted = complete::wanted(tt, chains);
    if wanted.is_empty() {
        return;
    }
    events(Event::Stage("Asking Transitous for the stops of trains known only from DB boards"));
    let found = join_all(wanted.iter().map(|&trip| complete::lookup(motis, tt, trip))).await;
    for (&trip, found) in wanted.iter().zip(found) {
        match found {
            Ok(Some(leg)) => {
                if let Some(moved) = timetable::add_stops(tt, trip, &leg) {
                    complete::remap(chains, trip, &moved);
                    let (before, label) = (moved.len(), tt.trip(trip).map(|t| (t.calls.len(), t.label.as_str())));
                    if let Ok((after, label)) = label {
                        tracing::debug!(train = label, before, after, "stops from Transitous");
                    }
                } else {
                    tracing::debug!(trip, "Transitous' copy adds no stops or disagrees");
                }
            }
            Ok(None) => {}
            Err(e) => {
                let label = tt.trip(trip).map(|t| t.label.clone()).unwrap_or_default();
                events(Event::Warning(&format!("stops of {label} from Transitous: {e:#}")));
            }
        }
    }
}

/// What a blocking job tells the search's events.
enum Update {
    Stage(&'static str),
    Progress(String),
}

/// Runs `job` on a blocking thread (CPU-bound work, keeping the runtime's
/// other tasks, e.g. a web server, responsive on any runtime), passing its
/// updates on to `events` as they come.
async fn blocking<T: Send + 'static>(
    events: &(dyn Fn(Event<'_>) + Sync),
    job: impl FnOnce(&UnboundedSender<Update>) -> Result<T> + Send + 'static,
) -> Result<T> {
    let (updates, mut received) = tokio::sync::mpsc::unbounded_channel();
    let handle = tokio::task::spawn_blocking(move || job(&updates));
    // Ends when the job drops its sender: finished, failed or panicked.
    while let Some(update) = received.recv().await {
        match update {
            Update::Stage(stage) => events(Event::Stage(stage)),
            Update::Progress(line) => events(Event::Progress(&line)),
        }
    }
    handle.await.context("the search thread failed")?
}

/// `work`, with what `status` says passed on as progress about every `REPORT_EVERY`.
async fn ticking<T>(
    work: impl Future<Output = T>,
    events: &(dyn Fn(Event<'_>) + Sync),
    status: impl Fn() -> String,
) -> T {
    let mut every = tokio::time::interval(REPORT_EVERY.max(Duration::from_millis(10)));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick is at once: nothing to say yet.
    every.tick().await;
    let mut work = std::pin::pin!(work);
    loop {
        // The work first: tokio::select! picks at random.
        let tick = std::pin::pin!(every.tick());
        match futures::future::select(work.as_mut(), tick).await {
            futures::future::Either::Left((done, _)) => return done,
            futures::future::Either::Right(_) => events(Event::Progress(&status())),
        }
    }
}

/// The services a search may ask besides Transitous; each `None` when off.
struct Feeds {
    efa: Option<efa::Efa>,
    db: Option<db::Db>,
    tv: Option<Trafikverket>,
    sj: Option<Sj>,
}

impl Feeds {
    /// EFA and SJ unless their endpoint is empty; DB and Trafikverket with a
    /// key, unless switched off.
    fn new(
        args: &SearchArgs,
        endpoints: &Endpoints,
        keys: Keys,
        user_agent: &str,
        events: &(dyn Fn(Event<'_>) + Sync),
    ) -> Result<Self> {
        let efa = (!endpoints.efa.is_empty()).then(|| efa::Efa::new(&endpoints.efa, user_agent)).transpose()?;
        let db = match (args.no_db, keys.db) {
            (false, Some(credentials)) => Some(db::Db::new(&endpoints.db_api, credentials, user_agent)?),
            (false, None) => {
                events(Event::Warning(
                    "no DB_CLIENT_ID/DB_API_KEY (env or ~/.config/umstieg/keys.env); using Transitous only",
                ));
                None
            }
            (true, _) => None,
        };
        let tv = match (args.no_trafikverket, keys.trafikverket) {
            (false, Some(key)) => Some(Trafikverket::new(&endpoints.trafikverket_api, &key, user_agent)?),
            _ => None,
        };
        let sj = (!endpoints.sj.is_empty()).then(|| Sj::new(&endpoints.sj, user_agent)).transpose()?;
        Ok(Self { efa, db, tv, sj })
    }

    /// "38 requests to Transitous, 6 to EFA, 80 to DB · waiting 40 s more for DB's limit of 60 requests per minute".
    fn so_far(&self, motis: &Motis) -> String {
        let efa = self.efa.as_ref().map(|efa| format!(", {} to EFA", efa.request_count())).unwrap_or_default();
        let db = self
            .db
            .as_ref()
            .map(|db| {
                let quota = db.waiting().map_or_else(String::new, |left| {
                    format!(" · waiting {} s more for DB's limit of 60 requests per minute", left.as_secs())
                });
                format!(", {} to DB{quota}", db.request_count())
            })
            .unwrap_or_default();
        let tv = self.tv.as_ref().map(|tv| format!(", {} to Trafikverket", tv.request_count())).unwrap_or_default();
        let sj = self.sj.as_ref().map(|sj| format!(", {} to SJ", sj.request_count())).unwrap_or_default();
        format!("{} requests to Transitous{efa}{db}{tv}{sj}", motis.request_count())
    }

    /// "12 Transitous requests, 3 EFA requests, …", for the report.
    fn summary(&self, motis: &Motis) -> String {
        let count = |n: Option<usize>, name: &str| n.map(|n| format!(", {n} {name} requests")).unwrap_or_default();
        format!(
            "{} Transitous requests{}{}{}{}",
            motis.request_count(),
            count(self.efa.as_ref().map(efa::Efa::request_count), "EFA"),
            count(self.db.as_ref().map(db::Db::request_count), "DB"),
            count(self.tv.as_ref().map(Trafikverket::request_count), "Trafikverket"),
            count(self.sj.as_ref().map(Sj::request_count), "SJ"),
        )
    }
}

/// SJ's connections through Sweden added to the rough routes; `true` if
/// there are any. A failure is a warning: SJ is an extra source.
async fn with_sj(
    feeds: &Feeds,
    query: &rough::Query,
    rough: &mut rough::Rough,
    events: &(dyn Fn(Event<'_>) + Sync),
    asked: impl Fn() -> String,
) -> bool {
    let Some(sj) = &feeds.sj else { return false };
    if sj::swedish_ends(query, &rough.itineraries).is_none() {
        return false;
    }
    events(Event::Stage("Asking SJ for connections through Sweden"));
    match ticking(sj::rough(sj, feeds.tv.as_ref(), query, &rough.itineraries), events, asked).await {
        Ok(found) => {
            let any = !found.is_empty();
            rough.itineraries.extend(found);
            any
        }
        Err(e) => {
            rough.warnings.push(format!("SJ: {e:#}"));
            false
        }
    }
}

/// `--extra-hours` as a duration, negative meaning none.
fn extra_time(hours: f64) -> Result<TimeDelta> {
    let seconds = hours * 3600.0;
    if !(seconds.is_finite() && seconds.abs() < 1e12) {
        bail!("--extra-hours: {hours} is out of range");
    }
    TimeDelta::try_seconds(timetable::whole_seconds(seconds).max(0))
        .with_context(|| format!("--extra-hours: {hours} is out of range"))
}

/// Everything the report needs, owned, to be written on a blocking thread.
struct Report {
    args: SearchArgs,
    built: timetable::Build,
    enumeration: enumerate::Enumeration,
    limits: Limits,
    hubs: Vec<String>,
    /// When the planner's earliest answer leaves and arrives.
    planner_first: Option<(DateTime<Utc>, DateTime<Utc>)>,
    /// "12 Transitous requests, 3 EFA requests, …".
    requests: String,
    with_db: bool,
    /// SJ's connections are among the rough routes.
    with_sj: bool,
    /// Where every time is shown in: --time-zone, else the machine's.
    zone: Tz,
    time: DateTime<Utc>,
    depart_until: DateTime<Utc>,
    min_transfer: TimeDelta,
    plot: Option<PlotPage>,
}

impl Report {
    /// Writes the report of the enumerated connections, sending the stages
    /// that follow, and progress, to `updates`.
    fn write(self, updates: &UnboundedSender<Update>) -> Result<Outcome> {
        let Self {
            args,
            built,
            enumeration: result,
            limits,
            hubs,
            planner_first,
            requests,
            with_db,
            with_sj,
            zone,
            time,
            depart_until,
            min_transfer,
            plot,
        } = self;
        let tt = &built.timetable;
        let net = Network::new(tt)?;
        let total = result.chains.len();
        let mut missed = 0_usize;
        for chain in &result.chains {
            if chain.min_slack(tt)?.is_some_and(|s| s < TimeDelta::zero()) {
                missed = missed.saturating_add(1);
            }
        }
        // What all of them use, drawn whether listed or not.
        let network = plot.is_some().then(|| usable::of_chains(tt, &result.chains));
        let listed = if args.all { result.chains } else { enumerate::best(tt, result.chains)? };
        let groups = enumerate::group(tt, listed)?;
        let listed = groups.iter().map(|g| g.chains.len()).sum();

        let mut out = String::new();
        let title = format!("{} → {}", tt.station(tt.origin)?.name, tt.station(tt.destination)?.name);
        let departing = format!(
            "departing {}{}",
            window(time, depart_until, zone),
            if args.planned { " · planned times, delays ignored" } else { "" }
        );
        writeln!(out, "{title}, {departing}")?;
        writeln!(
            out,
            "timetable: {} stations, {} trains · {} · {requests}",
            tt.stations.len(),
            tt.trips.len(),
            if hubs.is_empty() {
                "no change stations in the rough routes".to_owned()
            } else {
                format!("rough routes via {}", hubs.join(", "))
            },
        )?;
        if !built.discovered.is_empty() {
            writeln!(out, "stations found along trains' routes: {}", built.discovered.join(", "))?;
        }
        if built.sweden_from_transitous {
            writeln!(out, "Trafikverket announces trains about two weeks ahead: in Sweden, Transitous's departures")?;
        }
        let Some(fastest) = result.fastest else {
            writeln!(out, "\nNo connection found.{}", later(planner_first, zone))?;
            return Ok(Outcome { text: out, plot: None });
        };
        let (summary, completeness) =
            summarize(fastest, result.complete_up_to, &limits, (total, listed, groups.len()), missed);
        writeln!(out, "{summary} ({} states explored{completeness})\n", result.nodes)?;

        let mut fallbacks = Fallbacks::new(&net, min_transfer, zone);
        write_groups(&mut out, tt, &groups, &args, &mut fallbacks, min_transfer, zone)?;
        let times = if args.planned { "planned times" } else { "live data where available" };
        writeln!(out, "Data: Transitous, https://transitous.org/sources/ — {times}.")?;
        if with_db {
            writeln!(out, "      Deutsche Bahn Timetables API.")?;
        }
        if built.trafikverket {
            writeln!(out, "      Trafikverket open data (CC0).")?;
        }
        if with_sj {
            writeln!(out, "      SJ's connection search (sj.se).")?;
        }

        let rendered = match plot {
            Some(PlotPage { back, details_url }) => {
                // Only fails once `run` has stopped listening.
                updates.send(Update::Stage("Drawing the plot")).ok();
                let progress = |done: usize, total: usize| {
                    updates.send(Update::Progress(format!("{done} of {total} connections drawn"))).ok();
                };
                let plot = plot::Plot {
                    title,
                    usable: &network.unwrap_or_default(),
                    subtitle: format!("{departing} · {summary}{completeness}"),
                    groups: &groups,
                    metric: args.metric,
                    min_transfer,
                    zone,
                    back,
                    details_url,
                    progress: &progress,
                };
                Some(plot::render(&net, &mut fallbacks, &plot)?)
            }
            None => None,
        };
        Ok(Outcome { text: out, plot: rendered })
    }
}

/// The summary line, and how complete the enumeration is (", complete up to
/// 2 changes; raise --max-nodes for more", or nothing when it is).
fn summarize(
    fastest: TimeDelta,
    complete_up_to: Option<usize>,
    limits: &Limits,
    (total, listed, groups): (usize, usize, usize),
    missed: usize,
) -> (String, String) {
    let completeness = match complete_up_to {
        Some(n) if n >= limits.max_changes => String::new(),
        Some(n) => format!(", complete up to {n} changes; raise --max-nodes for more"),
        None => ", incomplete even without changes; raise --max-nodes".to_owned(),
    };
    let listing = if listed == total {
        format!("{total} connections in {groups} groups")
    } else {
        format!("{total} connections, the {listed} best in {groups} groups")
    };
    let summary = format!(
        "fastest {} · limits: ≤ {} travel time, ≤ {} changes · {listing}{}",
        duration(fastest),
        duration(checked::sum(fastest, limits.extra_time)),
        limits.max_changes,
        if missed > 0 {
            format!(", {missed} with a change missed by ≤ {}", duration(limits.missed_by))
        } else {
            String::new()
        },
    );
    (summary, completeness)
}

/// Each group, with its first `--per-group` connections, in detail or one line each.
fn write_groups(
    out: &mut String,
    tt: &timetable::Timetable,
    groups: &[enumerate::Group],
    args: &SearchArgs,
    fallbacks: &mut Fallbacks<'_>,
    min_transfer: TimeDelta,
    zone: Tz,
) -> Result<()> {
    for group in groups {
        writeln!(out, "■ {} … {}  ({})", tt.trip(group.first)?.label, tt.trip(group.last)?.label, group.chains.len())?;
        let shown = if args.per_group == 0 { group.chains.len() } else { args.per_group };
        for chain in group.chains.iter().take(shown) {
            if args.detail {
                write_detail(out, tt, chain, fallbacks, min_transfer, zone)?;
            } else {
                let route = route(tt, chain, min_transfer, &HashSet::new(), zone)?;
                writeln!(out, "  {}   {route}", header(tt, chain, min_transfer, zone)?)?;
            }
        }
        if group.chains.len() > shown {
            writeln!(out, "    … {} more", group.chains.len().saturating_sub(shown))?;
        }
        writeln!(out)?;
    }
    Ok(())
}

/// For `at` relative to `now` (nothing, "HH:MM"): the date and time it stands
/// for in the search's zone, as "YYYY-MM-DDTHH:MM", so that a link names one
/// search, not whichever "now" it is opened at. `None` for a dated `at`, or
/// one that won't parse.
pub fn dated_at(args: &SearchArgs, now: DateTime<Utc>) -> Option<String> {
    let at = args.at.as_deref().filter(|a| !a.is_empty());
    if at.is_some_and(|a| NaiveTime::parse_from_str(a, "%H:%M").is_err()) {
        return None;
    }
    let zone = match args.time_zone {
        Some(zone) => zone,
        None => machine_zone().ok()?,
    };
    let time = parse_time_in(at, zone, now).ok()?;
    Some(time.with_timezone(&zone).format("%Y-%m-%dT%H:%M").to_string())
}

/// `at` as a time in `zone`: "HH:MM" on `now`'s date there, nothing is `now`.
fn parse_time_in(at: Option<&str>, zone: Tz, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let Some(at) = at.filter(|a| !a.is_empty()) else { return Ok(now) };
    let naive = if let Ok(t) = NaiveTime::parse_from_str(at, "%H:%M") {
        NaiveDateTime::new(now.with_timezone(&zone).date_naive(), t)
    } else if let Ok(dt) = NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M") {
        dt
    } else if let Ok(dt) = NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M") {
        dt
    } else if let Ok(d) = NaiveDate::parse_from_str(at, "%Y-%m-%d") {
        d.and_time(NaiveTime::MIN)
    } else {
        bail!("cannot parse --at {at:?}; use \"HH:MM\" or \"YYYY-MM-DD HH:MM\"");
    };
    zone.from_local_datetime(&naive)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .with_context(|| format!("{at:?} does not exist in {zone}"))
}

/// Where a ride's times come from: the board feed ("DB", or "TV" for
/// Trafikverket) at both ends, Transitous ("T") at both, or a mix ("DB/T").
fn ride_source(board: &timetable::Call, alight: &timetable::Call) -> String {
    let feed = |c: &timetable::Call| match (c.from_board, c.trafikverket) {
        (true, true) => "TV",
        (true, false) => "DB",
        (false, _) => "T",
    };
    let (b, a) = (feed(board), feed(alight));
    if b == a {
        b.to_owned()
    } else if b == "T" {
        format!("{a}/{b}")
    } else {
        format!("{b}/{a}")
    }
}

/// One connection with platforms, and the fallbacks if a change is missed.
fn write_detail(
    out: &mut String,
    tt: &timetable::Timetable,
    chain: &Chain,
    fallbacks: &mut Fallbacks<'_>,
    min_transfer: TimeDelta,
    zone: Tz,
) -> Result<()> {
    writeln!(out, "  {}", header(tt, chain, min_transfer, zone)?)?;
    let transfers = chain.transfers(tt)?;
    let arrival = chain.arrival(tt)?;
    let mut ride = 0_usize;
    for step in &chain.steps {
        match *step {
            Step::Walk { footpath } => {
                let fp = tt.footpath(footpath)?;
                writeln!(out, "      walk {} to {}", duration(fp.duration), tt.station(fp.to)?.name)?;
            }
            Step::Ride { trip, board, alight } => {
                let t = tt.trip(trip)?;
                let (b, a) = (tt.call(trip, board)?, tt.call(trip, alight)?);
                let (bs, as_) = (tt.station(b.station)?, tt.station(a.station)?);
                let platform = |p: &Option<String>| p.as_deref().map(|p| format!(" [{p}]")).unwrap_or_default();
                let source = ride_source(b, a);
                writeln!(
                    out,
                    "    {} {}{} → {} {}{}   {}{}  ({source})",
                    timed(b.departure, zone),
                    bs.name,
                    platform(&b.platform),
                    timed(a.arrival.or(a.departure), zone),
                    as_.name,
                    platform(&a.platform),
                    t.label,
                    t.headsign.as_deref().map(|h| format!(" → {h}")).unwrap_or_default(),
                )?;
                if let Some(tr) = transfers.get(ride) {
                    writeln!(out, "      ↳ {} buffer · if missed:", buffer(tr.slack, min_transfer))?;
                    let options = fallbacks.for_change(chain, ride)?;
                    for f in &options {
                        let late = f.lands.signed_duration_since(arrival).max(TimeDelta::zero());
                        let lands = clock(f.lands, zone);
                        writeln!(out, "          {} · arrive {lands} (+{})", f.line, duration(late))?;
                    }
                    if options.is_empty() {
                        writeln!(out, "          nothing within {}", duration(fallback::WINDOW))?;
                    }
                }
                ride = ride.saturating_add(1);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
