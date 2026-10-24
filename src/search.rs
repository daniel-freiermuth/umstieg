//! One search, from stations and limits to the text report and the plot:
//! shared by the command line and `umstieg serve`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc};
use futures::future::try_join_all;

use crate::enumerate::{self, Chain, Limits, Network, Step};
use crate::fallback::{self, Fallbacks};
use crate::motis::Motis;
use crate::text::{buffer, clock, duration, header, route, timed};
use crate::{db, efa, plot, rough, timetable};

/// Stations, time and every limit of a search. The web form is generated from
/// these arguments, so options added here show up there too.
#[derive(clap::Args, Clone)]
pub struct SearchArgs {
    /// Origin station (free text).
    pub from: String,
    /// Destination station (free text).
    pub to: String,
    /// Earliest departure, local time: "HH:MM" (today) or "YYYY-MM-DD HH:MM". Default: now.
    #[arg(long)]
    pub at: Option<String>,
    /// Departure window in minutes.
    #[arg(long, default_value_t = 120)]
    pub window: i64,
    /// Maximum number of changes.
    #[arg(long, default_value_t = 15)]
    pub max_changes: usize,
    /// Show connections taking at most this many hours longer than the fastest.
    #[arg(long, default_value_t = 3.0)]
    pub extra_hours: f64,
    /// Changes shorter than this many minutes are flagged with ⚠.
    #[arg(long, default_value_t = 5)]
    pub min_transfer: i64,
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
}

pub struct Outcome {
    /// The text report, as the command line prints it.
    pub text: String,
    /// The plot page, if asked for.
    pub html: Option<String>,
}

/// Runs a search. `plot_back` asks for the plot page, with that link back to
/// the search form (`Some(None)`: plot without link).
pub async fn run(
    args: &SearchArgs,
    endpoints: &Endpoints,
    plot_back: Option<Option<String>>,
    events: &(dyn Fn(Event<'_>) + Sync),
) -> Result<Outcome> {
    let user_agent = endpoints.user_agent();
    let motis = Motis::new(&endpoints.api, &user_agent)?;
    let efa = (!endpoints.efa.is_empty()).then(|| efa::Efa::new(&endpoints.efa, &user_agent)).transpose()?;
    let db = match (args.no_db, db::Credentials::load()?) {
        (false, Some(credentials)) => Some(db::Db::new(credentials, &user_agent)?),
        (false, None) => {
            events(Event::Warning("no DB_CLIENT_ID/DB_API_KEY (env or ~/.config/umstieg/db.env); using Transitous only"));
            None
        }
        (true, _) => None,
    };

    let time = parse_time(args.at.as_deref())?;
    let window = TimeDelta::minutes(args.window);
    events(Event::Stage("Finding stations"));
    let (origin, destination) = tokio::try_join!(motis.stop(&args.from), motis.stop(&args.to))?;
    let vias = try_join_all(args.via.iter().map(|v| motis.stop(v))).await?;

    events(Event::Stage("Asking Transitous and EFA for rough routes"));
    let query = rough::Query { origin, destination, time, window, vias, auto_hubs: args.hubs };
    let rough = rough::collect(&motis, efa.as_ref(), &query).await?;
    events(Event::Stage("Building the timetable from DB station boards"));
    let mut built =
        timetable::build(&motis, db.as_ref(), &rough.itineraries, &query.origin, &query.destination, args.all_stops)
            .await;
    if args.planned {
        built.timetable.ignore_delays();
    }
    for warning in rough.warnings.iter().chain(&built.warnings) {
        events(Event::Warning(warning));
    }

    events(Event::Stage("Enumerating connections"));
    // CPU-bound from here on; keep the runtime's other tasks (e.g. a web server) responsive.
    tokio::task::block_in_place(|| {
        let tt = &built.timetable;
        let net = Network::new(tt);
        let min_transfer = TimeDelta::minutes(args.min_transfer);
        let limits = Limits {
            origin: tt.origin,
            depart_from: time,
            depart_until: time + window,
            max_changes: args.max_changes,
            extra_time: TimeDelta::seconds((args.extra_hours * 3600.0) as i64),
            min_transfer,
            keep_detours: args.keep_detours,
            forward_only: args.forward_only,
            max_nodes: args.max_nodes,
        };
        let result = enumerate::enumerate(&net, &limits);
        let total = result.chains.len();
        let groups = enumerate::group(tt, result.chains);

        let mut out = String::new();
        let title = format!("{} → {}", tt.stations[tt.origin].name, tt.stations[tt.destination].name);
        let departing = format!(
            "departing {}–{}{}",
            clock(time, None),
            clock(time + window, None),
            if args.planned { " · planned times, delays ignored" } else { "" }
        );
        writeln!(out, "{title}, {departing}")?;
        writeln!(
            out,
            "timetable: {} stations, {} trains · rough routes via {} · {} Transitous requests{}{}",
            tt.stations.len(),
            tt.trips.len(),
            rough.hubs.join(", "),
            motis.request_count(),
            efa.as_ref().map(|efa| format!(", {} EFA requests", efa.request_count())).unwrap_or_default(),
            db.as_ref().map(|db| format!(", {} DB requests", db.request_count())).unwrap_or_default(),
        )?;
        if !built.discovered.is_empty() {
            writeln!(out, "stations found via DB train paths: {}", built.discovered.join(", "))?;
        }
        let Some(fastest) = result.fastest else {
            writeln!(out, "\nNo connection found.")?;
            return Ok(Outcome { text: out, html: None });
        };
        let completeness = match result.complete_up_to {
            Some(n) if n >= limits.max_changes => String::new(),
            Some(n) => format!(", complete up to {n} changes; raise --max-nodes for more"),
            None => ", incomplete even without changes; raise --max-nodes".to_owned(),
        };
        let summary = format!(
            "fastest {} · limits: ≤ {} travel time, ≤ {} changes · {} connections in {} groups",
            duration(fastest),
            duration(fastest + limits.extra_time),
            limits.max_changes,
            total,
            groups.len(),
        );
        writeln!(out, "{summary} ({} states explored{completeness})\n", result.nodes)?;

        let mut fallbacks = Fallbacks::new(&net, min_transfer);
        for group in &groups {
            writeln!(out, "■ {} … {}  ({})", tt.trips[group.first].label, tt.trips[group.last].label, group.chains.len())?;
            let shown = if args.per_group == 0 { group.chains.len() } else { args.per_group };
            for chain in group.chains.iter().take(shown) {
                if args.detail {
                    write_detail(&mut out, tt, chain, &mut fallbacks, min_transfer)?;
                } else {
                    let route = route(tt, chain, min_transfer, &HashSet::new());
                    writeln!(out, "  {}   {route}", header(tt, chain, min_transfer))?;
                }
            }
            if group.chains.len() > shown {
                writeln!(out, "    … {} more", group.chains.len() - shown)?;
            }
            writeln!(out)?;
        }
        let times = if args.planned { "planned times" } else { "live data where available" };
        writeln!(out, "Data: Transitous, https://transitous.org/sources/ — {times}.")?;
        if db.is_some() {
            writeln!(out, "      Deutsche Bahn Timetables API.")?;
        }

        let html = match plot_back {
            Some(back) => {
                events(Event::Stage("Drawing the plot"));
                let plot = plot::Plot {
                    title,
                    subtitle: format!("{departing} · {summary}{completeness}"),
                    groups: &groups,
                    metric: args.metric,
                    min_transfer,
                    back,
                };
                Some(plot::render(&net, &mut fallbacks, &plot)?)
            }
            None => None,
        };
        Ok(Outcome { text: out, html })
    })
}

fn parse_time(at: Option<&str>) -> Result<DateTime<Utc>> {
    let Some(at) = at.filter(|a| !a.is_empty()) else { return Ok(Utc::now()) };
    let naive = if let Ok(t) = NaiveTime::parse_from_str(at, "%H:%M") {
        NaiveDateTime::new(Local::now().date_naive(), t)
    } else if let Ok(dt) = NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M") {
        dt
    } else if let Ok(dt) = NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M") {
        dt
    } else if let Ok(d) = NaiveDate::parse_from_str(at, "%Y-%m-%d") {
        d.and_time(NaiveTime::MIN)
    } else {
        bail!("cannot parse --at {at:?}; use \"HH:MM\" or \"YYYY-MM-DD HH:MM\"");
    };
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .with_context(|| format!("{at:?} does not exist in the local time zone"))
}

/// One connection with platforms, and the fallbacks if a change is missed.
fn write_detail(
    out: &mut String,
    tt: &timetable::Timetable,
    chain: &Chain,
    fallbacks: &mut Fallbacks<'_>,
    min_transfer: TimeDelta,
) -> std::fmt::Result {
    writeln!(out, "  {}", header(tt, chain, min_transfer))?;
    let transfers = chain.transfers(tt);
    let mut ride = 0;
    for step in &chain.steps {
        match *step {
            Step::Walk { footpath } => {
                let fp = &tt.footpaths[footpath];
                writeln!(out, "      walk {} to {}", duration(fp.duration), tt.stations[fp.to].name)?;
            }
            Step::Ride { trip, board, alight } => {
                let t = &tt.trips[trip];
                let (b, a) = (&t.calls[board], &t.calls[alight]);
                let (bs, as_) = (&tt.stations[b.station], &tt.stations[a.station]);
                let platform = |p: &Option<String>| p.as_deref().map(|p| format!(" [{p}]")).unwrap_or_default();
                let source = if b.from_db && a.from_db { "DB" } else if b.from_db || a.from_db { "DB/T" } else { "T" };
                writeln!(
                    out,
                    "    {} {}{} → {} {}{}   {}{}  ({source})",
                    timed(b.departure, bs.tz.as_deref()),
                    bs.name,
                    platform(&b.platform),
                    timed(a.arrival.or(a.departure), as_.tz.as_deref()),
                    as_.name,
                    platform(&a.platform),
                    t.label,
                    t.headsign.as_deref().map(|h| format!(" → {h}")).unwrap_or_default(),
                )?;
                if let Some(tr) = transfers.get(ride) {
                    writeln!(out, "      ↳ {} buffer · if missed:", buffer(tr.slack, min_transfer))?;
                    let arrival = chain.arrival(tt);
                    let options = fallbacks.for_change(chain, ride);
                    for f in &options {
                        let late = (f.lands - arrival).max(TimeDelta::zero());
                        let tz = tt.stations[tt.destination].tz.as_deref();
                        writeln!(out, "          {} · arrive {} (+{})", f.line, clock(f.lands, tz), duration(late))?;
                    }
                    if options.is_empty() {
                        writeln!(out, "          nothing within {}", duration(fallback::WINDOW))?;
                    }
                }
                ride += 1;
            }
        }
    }
    Ok(())
}
