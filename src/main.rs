mod db;
mod enumerate;
mod fallback;
mod motis;
mod plot;
mod rough;
mod text;
mod timetable;

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc};
use clap::Parser;
use futures::future::try_join_all;

use enumerate::{Chain, Limits, Network, Step};
use fallback::Fallbacks;
use motis::Motis;
use text::{buffer, clock, duration, header, route, timed};

/// Lists every public transport connection within limits, including those
/// journey planners hide, grouped by first and last train.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Origin station (free text).
    from: String,
    /// Destination station (free text).
    to: String,
    /// Earliest departure, local time: "HH:MM" (today) or "YYYY-MM-DD HH:MM". Default: now.
    #[arg(long)]
    at: Option<String>,
    /// Departure window in minutes.
    #[arg(long, default_value_t = 120)]
    window: i64,
    /// Maximum number of changes.
    #[arg(long, default_value_t = 15)]
    max_changes: usize,
    /// Show connections taking at most this many hours longer than the fastest.
    #[arg(long, default_value_t = 3.0)]
    extra_hours: f64,
    /// Changes shorter than this many minutes are flagged with ⚠.
    #[arg(long, default_value_t = 5)]
    min_transfer: i64,
    /// Connections listed per (first train, last train) group; 0 = all.
    #[arg(long, default_value_t = 5)]
    per_group: usize,
    /// Show every connection with platforms and what happens if a change is missed.
    #[arg(long)]
    detail: bool,
    /// Write an interactive time–distance plot of all connections to this HTML file.
    #[arg(long)]
    html: Option<PathBuf>,
    /// Height of a station in the plot.
    #[arg(long, value_enum, default_value_t = plot::Metric::RemainingTime)]
    metric: plot::Metric,
    /// Station to route the rough search through; repeatable.
    #[arg(long)]
    via: Vec<String>,
    /// Transfer stations of the planner's answers used to widen the rough search.
    #[arg(long, default_value_t = 6)]
    hubs: usize,
    /// Also list connections that change to a train they could have boarded at an earlier stop.
    #[arg(long)]
    keep_detours: bool,
    /// Only rides that reduce the best-case remaining travel time (no riding away
    /// from the destination): fewer, more direct connections, much faster search.
    #[arg(long)]
    forward_only: bool,
    /// Search effort cap (explored states; ~1 s per million).
    #[arg(long, default_value_t = 20_000_000)]
    max_nodes: usize,
    /// Don't use DB's station boards even if credentials are configured.
    #[arg(long)]
    no_db: bool,
    /// Fetch DB boards for every stop of the rough routes' trains, not only where
    /// they start or end: more change points, ~3× the DB requests on a cold cache.
    #[arg(long)]
    all_stops: bool,
    /// MOTIS API base URL.
    #[arg(long, default_value = "https://api.transitous.org")]
    api: String,
    /// Contact sent in the User-Agent, as the Transitous usage policy requires.
    #[arg(long, env = "UMSTIEG_CONTACT", default_value = "https://github.com/daniel-freiermuth")]
    contact: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let user_agent = format!("umstieg/{} ({})", env!("CARGO_PKG_VERSION"), cli.contact);
    let motis = Motis::new(&cli.api, &user_agent)?;
    let db = match (cli.no_db, db::Credentials::load()?) {
        (false, Some(credentials)) => Some(db::Db::new(credentials, &user_agent)?),
        (false, None) => {
            eprintln!("note: no DB_CLIENT_ID/DB_API_KEY (env or ~/.config/umstieg/db.env); using Transitous only");
            None
        }
        (true, _) => None,
    };

    let time = parse_time(cli.at.as_deref())?;
    let window = TimeDelta::minutes(cli.window);
    let (origin, destination) = tokio::try_join!(motis.stop(&cli.from), motis.stop(&cli.to))?;
    let vias = try_join_all(cli.via.iter().map(|v| motis.stop(v))).await?;

    let query = rough::Query { origin, destination, time, window, vias, auto_hubs: cli.hubs };
    let rough = rough::collect(&motis, &query).await?;
    let built = timetable::build(
        &motis,
        db.as_ref(),
        &rough.itineraries,
        &query.origin,
        &query.destination,
        cli.all_stops,
    )
    .await;
    let tt = &built.timetable;
    let net = Network::new(tt);

    let min_transfer = TimeDelta::minutes(cli.min_transfer);
    let limits = Limits {
        origin: tt.origin,
        depart_from: time,
        depart_until: time + window,
        max_changes: cli.max_changes,
        extra_time: TimeDelta::seconds((cli.extra_hours * 3600.0) as i64),
        min_transfer,
        keep_detours: cli.keep_detours,
        forward_only: cli.forward_only,
        max_nodes: cli.max_nodes,
    };
    let result = enumerate::enumerate(&net, &limits);
    let total = result.chains.len();
    let groups = enumerate::group(tt, result.chains);

    for warning in rough.warnings.iter().chain(&built.warnings) {
        eprintln!("warning: {warning}");
    }
    let title = format!("{} → {}", tt.stations[tt.origin].name, tt.stations[tt.destination].name);
    let departing = format!("departing {}–{}", clock(time, None), clock(time + window, None));
    println!("{title}, {departing}");
    println!(
        "timetable: {} stations, {} trains · rough routes via {} · {} Transitous requests{}",
        tt.stations.len(),
        tt.trips.len(),
        rough.hubs.join(", "),
        motis.request_count(),
        db.as_ref()
            .map(|db| format!(", {} DB requests", db.request_count()))
            .unwrap_or_default(),
    );
    if !built.discovered.is_empty() {
        println!("stations found via DB train paths: {}", built.discovered.join(", "));
    }
    let Some(fastest) = result.fastest else {
        println!("\nNo connection found.");
        return Ok(());
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
    println!("{summary} ({} states explored{completeness})\n", result.nodes);

    let mut fallbacks = Fallbacks::new(&net, min_transfer);
    for group in &groups {
        println!(
            "■ {} … {}  ({})",
            tt.trips[group.first].label,
            tt.trips[group.last].label,
            group.chains.len()
        );
        let shown = if cli.per_group == 0 { group.chains.len() } else { cli.per_group };
        for chain in group.chains.iter().take(shown) {
            if cli.detail {
                print_detail(tt, chain, &mut fallbacks, min_transfer);
            } else {
                println!("  {}   {}", header(tt, chain, min_transfer), route(tt, chain, min_transfer, &HashSet::new()));
            }
        }
        if group.chains.len() > shown {
            println!("    … {} more", group.chains.len() - shown);
        }
        println!();
    }
    println!("Data: Transitous, https://transitous.org/sources/ — live data where available.");
    if db.is_some() {
        println!("      Deutsche Bahn Timetables API.");
    }

    if let Some(path) = &cli.html {
        let plot = plot::Plot {
            title,
            subtitle: format!("{departing} · {summary}{completeness}"),
            groups: &groups,
            metric: cli.metric,
            min_transfer,
        };
        plot::write(path, &net, &mut fallbacks, &plot)?;
        println!("\nPlot written to {}", path.display());
    }
    Ok(())
}

fn parse_time(at: Option<&str>) -> Result<DateTime<Utc>> {
    let Some(at) = at else { return Ok(Utc::now()) };
    let naive = if let Ok(t) = NaiveTime::parse_from_str(at, "%H:%M") {
        NaiveDateTime::new(Local::now().date_naive(), t)
    } else if let Ok(dt) = NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M") {
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
fn print_detail(tt: &timetable::Timetable, chain: &Chain, fallbacks: &mut Fallbacks<'_>, min_transfer: TimeDelta) {
    println!("  {}", header(tt, chain, min_transfer));
    let transfers = chain.transfers(tt);
    let mut ride = 0;
    for step in &chain.steps {
        match *step {
            Step::Walk { footpath } => {
                let fp = &tt.footpaths[footpath];
                println!("      walk {} to {}", duration(fp.duration), tt.stations[fp.to].name);
            }
            Step::Ride { trip, board, alight } => {
                let t = &tt.trips[trip];
                let (b, a) = (&t.calls[board], &t.calls[alight]);
                let (bs, as_) = (&tt.stations[b.station], &tt.stations[a.station]);
                let platform = |p: &Option<String>| p.as_deref().map(|p| format!(" [{p}]")).unwrap_or_default();
                let source = if b.from_db && a.from_db { "DB" } else if b.from_db || a.from_db { "DB/T" } else { "T" };
                println!(
                    "    {} {}{} → {} {}{}   {}{}  ({source})",
                    timed(b.departure, bs.tz.as_deref()),
                    bs.name,
                    platform(&b.platform),
                    timed(a.arrival.or(a.departure), as_.tz.as_deref()),
                    as_.name,
                    platform(&a.platform),
                    t.label,
                    t.headsign.as_deref().map(|h| format!(" → {h}")).unwrap_or_default(),
                );
                if let Some(tr) = transfers.get(ride) {
                    println!("      ↳ {} buffer · if missed:", buffer(tr.slack, min_transfer));
                    let arrival = chain.arrival(tt);
                    let options = fallbacks.for_change(chain, ride);
                    for f in &options {
                        let late = (f.lands - arrival).max(TimeDelta::zero());
                        let tz = tt.stations[tt.destination].tz.as_deref();
                        println!("          {} · arrive {} (+{})", f.line, clock(f.lands, tz), duration(late));
                    }
                    if options.is_empty() {
                        println!("          nothing within {}", duration(fallback::WINDOW));
                    }
                }
                ride += 1;
            }
        }
    }
}
