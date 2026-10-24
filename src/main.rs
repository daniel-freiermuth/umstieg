mod db;
mod enumerate;
mod motis;
mod rough;
mod timetable;

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use clap::Parser;
use futures::future::try_join_all;

use enumerate::{Chain, Limits, Step};
use motis::Motis;
use timetable::{Time, Timetable};

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
    /// Station to route the rough search through; repeatable.
    #[arg(long)]
    via: Vec<String>,
    /// Transfer stations of the planner's answers used to widen the rough search.
    #[arg(long, default_value_t = 6)]
    hubs: usize,
    /// Also list connections that change to a train they could have boarded at an earlier stop.
    #[arg(long)]
    keep_detours: bool,
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

    let min_transfer = TimeDelta::minutes(cli.min_transfer);
    let limits = Limits {
        origin: tt.origin,
        depart_from: time,
        depart_until: time + window,
        max_changes: cli.max_changes,
        extra_time: TimeDelta::seconds((cli.extra_hours * 3600.0) as i64),
        min_transfer,
        keep_detours: cli.keep_detours,
        max_nodes: cli.max_nodes,
    };
    let result = enumerate::enumerate(tt, &limits);
    let total = result.chains.len();
    let groups = enumerate::group(tt, result.chains);

    for warning in rough.warnings.iter().chain(&built.warnings) {
        eprintln!("warning: {warning}");
    }
    println!(
        "{} → {}, departing {}–{}",
        tt.stations[tt.origin].name,
        tt.stations[tt.destination].name,
        clock(time, None),
        clock(time + window, None),
    );
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
    println!(
        "fastest {} · limits: ≤ {} travel time, ≤ {} changes · {} connections in {} groups ({} states explored{completeness})\n",
        duration(fastest),
        duration(fastest + limits.extra_time),
        limits.max_changes,
        total,
        groups.len(),
        result.nodes,
    );

    let mut fallbacks = Fallbacks { min_transfer, cache: HashMap::new() };
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
                print_detail(tt, chain, &mut fallbacks);
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

/// "HH:MM" in the station's time zone, or local time if unknown.
fn clock(t: DateTime<Utc>, tz: Option<&str>) -> String {
    match tz.and_then(|tz| tz.parse::<Tz>().ok()) {
        Some(tz) => t.with_timezone(&tz).format("%H:%M").to_string(),
        None => t.with_timezone(&Local).format("%H:%M").to_string(),
    }
}

fn duration(d: TimeDelta) -> String {
    let m = d.num_minutes();
    if m.abs() >= 60 {
        format!("{}h{:02}", m / 60, (m % 60).abs())
    } else {
        format!("{m}m")
    }
}

/// Time plus live delay, e.g. "11:51" or "11:55 (+4)".
fn timed(t: Option<Time>, tz: Option<&str>) -> String {
    let Some(t) = t else { return "--:--".to_owned() };
    let delay = t.delay().num_minutes();
    if delay != 0 {
        format!("{} ({delay:+})", clock(t.actual, tz))
    } else {
        clock(t.actual, tz)
    }
}

fn buffer(slack: TimeDelta, min_transfer: TimeDelta) -> String {
    let warn = if slack < min_transfer { "⚠" } else { "" };
    format!("{}{warn}", duration(slack))
}

fn header(tt: &Timetable, chain: &Chain, min_transfer: TimeDelta) -> String {
    let departure = chain.departure(tt);
    let arrival = chain.arrival(tt);
    let min = chain
        .min_slack(tt)
        .map_or_else(|| "direct".to_owned(), |s| format!("min {}", buffer(s, min_transfer)));
    format!(
        "{} → {}  {}  {}×  {}",
        clock(departure, tt.stations[tt.origin].tz.as_deref()),
        clock(arrival, tt.stations[tt.destination].tz.as_deref()),
        duration(arrival - departure),
        chain.changes(),
        min,
    )
}

/// Trains, change stations and buffers on one line. Stops at the first train
/// in `rejoin`, marking it as back on the original plan.
fn route(tt: &Timetable, chain: &Chain, min_transfer: TimeDelta, rejoin: &HashSet<usize>) -> String {
    let transfers = chain.transfers(tt);
    let mut parts = Vec::new();
    for (i, (trip, board, alight)) in chain.rides().enumerate() {
        let t = &tt.trips[trip];
        let (b, a) = (&t.calls[board], &t.calls[alight]);
        let (bs, as_) = (&tt.stations[b.station], &tt.stations[a.station]);
        if i > 0 {
            let tr = &transfers[i - 1];
            let walk = if tr.walk > TimeDelta::zero() { format!(" walk {}", duration(tr.walk)) } else { String::new() };
            parts.push(format!("‖{}{walk}‖", buffer(tr.slack, min_transfer)));
        }
        let from = if i > 0 && bs.name != tt.stations[transfers[i - 1].station].name {
            format!(" {}", bs.name)
        } else {
            String::new()
        };
        if rejoin.contains(&trip) {
            parts.push(format!("{}{from} {} (back on plan)", timed(b.departure, bs.tz.as_deref()), t.label));
            break;
        }
        parts.push(format!(
            "{}{from} {} → {} {}",
            timed(b.departure, bs.tz.as_deref()),
            t.label,
            timed(a.arrival.or(a.departure), as_.tz.as_deref()),
            as_.name,
        ));
    }
    parts.join(" ")
}

/// How long after a missed train fallbacks may leave, and how they're bounded.
const FALLBACK_WINDOW: TimeDelta = TimeDelta::hours(2);
const FALLBACK_EXTRA: TimeDelta = TimeDelta::hours(1);
const FALLBACK_CHANGES: usize = 3;
const FALLBACK_NODES: usize = 500_000;
const FALLBACKS_SHOWN: usize = 3;

/// Fallback connections per (station, missed departure), computed once.
struct Fallbacks {
    min_transfer: TimeDelta,
    cache: HashMap<(usize, DateTime<Utc>), Vec<Chain>>,
}

impl Fallbacks {
    /// Connections from `station` leaving after `missed`, best first.
    fn from(&mut self, tt: &Timetable, station: usize, missed: DateTime<Utc>) -> &[Chain] {
        let min_transfer = self.min_transfer;
        self.cache.entry((station, missed)).or_insert_with(|| {
            let after = missed + TimeDelta::minutes(1);
            let limits = Limits {
                origin: station,
                depart_from: after,
                depart_until: after + FALLBACK_WINDOW,
                max_changes: FALLBACK_CHANGES,
                extra_time: FALLBACK_EXTRA,
                min_transfer,
                keep_detours: false,
                max_nodes: FALLBACK_NODES,
            };
            let mut chains = enumerate::enumerate(tt, &limits).chains;
            chains.sort_by_key(|c| (c.arrival(tt), c.changes(), std::cmp::Reverse(c.min_slack(tt))));
            chains
        })
    }
}

/// One connection with platforms, and the fallbacks if a change is missed.
fn print_detail(tt: &Timetable, chain: &Chain, fallbacks: &mut Fallbacks) {
    let min_transfer = fallbacks.min_transfer;
    println!("  {}", header(tt, chain, min_transfer));
    let transfers = chain.transfers(tt);
    let trips = chain.trips();
    let arrival = chain.arrival(tt);
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
                    // Catching a later train of the plan (also the missed one, further on) ends the detour.
                    let rejoin: HashSet<usize> = trips[ride + 1..].iter().copied().collect();
                    let options = fallbacks.from(tt, tr.station, tr.departure);
                    // Rank by what happens until the plan is rejoined: from there on the
                    // trip continues as planned, so its arrival is the planned one.
                    let mut ranked: Vec<_> = options
                        .iter()
                        .map(|fallback| {
                            let rides = fallback.trips();
                            let back = rides.iter().position(|t| rejoin.contains(t));
                            let changes = back.unwrap_or(rides.len() - 1);
                            let slack = fallback.transfers(tt)[..changes].iter().map(|t| t.slack).min();
                            let lands = if back.is_some() { arrival } else { fallback.arrival(tt) };
                            (lands, changes, std::cmp::Reverse(slack), route(tt, fallback, min_transfer, &rejoin))
                        })
                        .collect();
                    ranked.sort();
                    ranked.dedup_by(|a, b| a.3 == b.3);
                    for (lands, _, _, line) in ranked.iter().take(FALLBACKS_SHOWN) {
                        let late = (*lands - arrival).max(TimeDelta::zero());
                        let tz = tt.stations[tt.destination].tz.as_deref();
                        println!("          {line} · arrive {} (+{})", clock(*lands, tz), duration(late));
                    }
                    if ranked.is_empty() {
                        println!("          nothing within {}", duration(FALLBACK_WINDOW));
                    }
                }
                ride += 1;
            }
        }
    }
}
