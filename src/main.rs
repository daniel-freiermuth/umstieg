mod db;
mod motis;
mod search;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use clap::Parser;
use futures::future::try_join_all;

use motis::{Leg, Motis};
use search::{Evaluated, Journey, Query, Recovery};

/// Find public transport connections that journey planners hide, with their
/// transfer buffers and what happens if you miss a change.
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
    /// Station to split the trip at; repeatable. Added to the automatic hubs.
    #[arg(long)]
    via: Vec<String>,
    /// Number of transfer stations taken from the planner's own answers as hubs.
    #[arg(long, default_value_t = 6)]
    hubs: usize,
    /// Minimum buffer in minutes for changes assembled at a hub.
    #[arg(long, default_value_t = 5)]
    min_transfer: i64,
    /// Buffers above this many minutes count as equally comfortable.
    #[arg(long, default_value_t = 30)]
    enough_buffer: i64,
    /// Skip the "what if I miss this change" queries.
    #[arg(long)]
    no_fallbacks: bool,
    /// Skip checking trains against DB's live data even if credentials are configured.
    #[arg(long)]
    no_db: bool,
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
            eprintln!("note: no DB_CLIENT_ID/DB_API_KEY (env or ~/.config/umstieg/db.env); skipping DB check");
            None
        }
        (true, _) => None,
    };

    let time = parse_time(cli.at.as_deref())?;
    let (origin, destination) = tokio::try_join!(motis.stop(&cli.from), motis.stop(&cli.to))?;
    let vias = try_join_all(cli.via.iter().map(|v| motis.stop(v))).await?;

    let query = Query {
        origin,
        destination,
        time,
        window: TimeDelta::minutes(cli.window),
        vias,
        auto_hubs: cli.hubs,
        min_transfer: TimeDelta::minutes(cli.min_transfer),
        enough_buffer: TimeDelta::minutes(cli.enough_buffer),
    };
    let outcome = search::search(&motis, db.as_ref(), &query, !cli.no_fallbacks).await?;

    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }
    println!(
        "{} → {}   departing {}–{}",
        query.origin.name,
        query.destination.name,
        clock(time, None),
        clock(time + query.window, None),
    );
    println!(
        "hubs: {}\n{} candidates, {} worth showing ({} Transitous requests{})\n",
        outcome.hubs.join(", "),
        outcome.candidates,
        outcome.journeys.len(),
        motis.request_count(),
        db.as_ref()
            .map(|db| format!(", {} DB requests", db.request_count()))
            .unwrap_or_default(),
    );
    for evaluated in &outcome.journeys {
        print_journey(evaluated);
    }
    if !outcome.rejected.is_empty() {
        println!("Dropped after DB check:");
        for r in &outcome.rejected {
            println!("  ✗ {}: {}", summary(&r.journey), r.reason);
        }
        println!();
    }
    println!("Data: Transitous, https://transitous.org/sources/ — live data where available.");
    if db.is_some() {
        println!("      Deutsche Bahn Timetables API (DB ✓ = confirmed by DB).");
    }
    Ok(())
}

/// One line: times and trains.
fn summary(j: &Journey) -> String {
    let trains: Vec<&str> = j.legs.iter().filter(|l| l.is_transit()).map(Leg::label).collect();
    format!(
        "{} → {} {}",
        clock(j.departure(), j.legs[0].from.tz.as_deref()),
        clock(j.arrival(), j.legs[j.legs.len() - 1].to.tz.as_deref()),
        trains.join(" / "),
    )
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

fn minutes(d: TimeDelta) -> String {
    let m = d.num_minutes();
    if m.abs() >= 60 {
        format!("{}h{:02}", m / 60, (m % 60).abs())
    } else {
        format!("{m}m")
    }
}

/// Time plus live delay, e.g. "11:51" or "11:55 (+4)".
fn timed(actual: DateTime<Utc>, scheduled: DateTime<Utc>, live: bool, tz: Option<&str>) -> String {
    let delay = (actual - scheduled).num_minutes();
    if live && delay != 0 {
        format!("{} ({delay:+})", clock(actual, tz))
    } else {
        clock(actual, tz)
    }
}

fn place(name: &str, track: Option<&str>) -> String {
    match track {
        Some(track) => format!("{name} [{track}]"),
        None => name.to_owned(),
    }
}

fn print_journey(e: &Evaluated) {
    let j = &e.journey;
    let tz = j.legs[0].from.tz.as_deref();
    let buffer = j
        .min_slack()
        .map_or_else(|| "direct".to_owned(), |s| format!("min buffer {}", minutes(s)));
    let origin = if j.planner_shows {
        String::new()
    } else {
        format!(
            "   ★ hidden by planner{}",
            j.via_hub.as_deref().map(|h| format!(", found via {h}")).unwrap_or_default()
        )
    };
    println!(
        "{} → {}  {}  {} change(s), {}{}",
        clock(j.departure(), tz),
        clock(j.arrival(), j.legs[j.legs.len() - 1].to.tz.as_deref()),
        minutes(j.arrival() - j.departure()),
        j.changes(),
        buffer,
        origin,
    );

    let transfers = j.transfers();
    let mut transit_index = 0;
    for leg in &j.legs {
        if !leg.is_transit() {
            println!("        walk {}", minutes(leg.duration()));
            continue;
        }
        print_leg(leg);
        if let Some(t) = transfers.get(transit_index) {
            let missed = match e.recoveries.get(transit_index) {
                None => String::new(),
                Some(r) => format!(" · if missed: {}", describe(r, j.arrival())),
            };
            println!("        ↳ {} buffer{missed}", minutes(t.slack));
        }
        transit_index += 1;
    }
    println!();
}

fn print_leg(leg: &Leg) {
    let from_tz = leg.from.tz.as_deref();
    let to_tz = leg.to.tz.as_deref();
    let towards = leg.headsign.as_deref().map(|h| format!(" → {h}")).unwrap_or_default();
    let db = match (leg.confirmed.departure, leg.confirmed.arrival) {
        (true, true) => "   DB ✓",
        (true, false) => "   DB ✓ dep",
        (false, true) => "   DB ✓ arr",
        (false, false) => "",
    };
    println!(
        "  {}  {}  →  {}  {}   {}{}{}",
        timed(leg.start_time, leg.scheduled_start_time, leg.real_time, from_tz),
        place(&leg.from.name, leg.from.track.as_deref()),
        timed(leg.end_time, leg.scheduled_end_time, leg.real_time, to_tz),
        place(&leg.to.name, leg.to.track.as_deref()),
        leg.label(),
        towards,
        db,
    );
}

fn describe(r: &Recovery, planned_arrival: DateTime<Utc>) -> String {
    match r {
        Recovery::Reroute { arrival, first_leg } => {
            let late = (*arrival - planned_arrival).max(TimeDelta::zero());
            format!(
                "arrive {} (+{}), next: {} {} from {}",
                clock(*arrival, None),
                minutes(late),
                first_leg.label(),
                clock(first_leg.start_time, first_leg.from.tz.as_deref()),
                first_leg.from.name,
            )
        }
        Recovery::Stranded => "nothing within 4h".to_owned(),
        Recovery::Failed(e) => format!("lookup failed ({e})"),
    }
}
