//! Text formatting shared by the terminal output and the HTML plot.

use std::collections::HashSet;

use chrono::{DateTime, Local, TimeDelta, Utc};
use chrono_tz::Tz;

use crate::enumerate::Chain;
use crate::timetable::{Time, Timetable};

/// "HH:MM" in the station's time zone, or local time if unknown.
pub fn clock(t: DateTime<Utc>, tz: Option<&str>) -> String {
    let format = "%H:%M";
    tz.and_then(|tz| tz.parse::<Tz>().ok()).map_or_else(
        || t.with_timezone(&Local).format(format).to_string(),
        |tz| t.with_timezone(&tz).format(format).to_string(),
    )
}

pub fn duration(d: TimeDelta) -> String {
    let m = d.num_minutes();
    if m.abs() >= 60 { format!("{}h{:02}", m / 60, (m % 60).abs()) } else { format!("{m}m") }
}

/// Time plus live delay, e.g. "11:51" or "11:55 (+4)".
pub fn timed(t: Option<Time>, tz: Option<&str>) -> String {
    let Some(t) = t else { return "--:--".to_owned() };
    let delay = t.delay().num_minutes();
    if delay != 0 { format!("{} ({delay:+})", clock(t.actual, tz)) } else { clock(t.actual, tz) }
}

/// "12m", "3m⚠" below the minimum transfer, "missed 2m✗" for a change that
/// live data says is gone (listed with `--missed-by`: delays may still shift).
pub fn buffer(slack: TimeDelta, min_transfer: TimeDelta) -> String {
    if slack < TimeDelta::zero() {
        return format!("missed {}✗", duration(-slack));
    }
    let warn = if slack < min_transfer { "⚠" } else { "" };
    format!("{}{warn}", duration(slack))
}

/// "06:39 → 22:05  15h26  3×  min 47m"
pub fn header(tt: &Timetable, chain: &Chain, min_transfer: TimeDelta) -> String {
    let departure = chain.departure(tt);
    let arrival = chain.arrival(tt);
    let min = chain.min_slack(tt).map_or_else(|| "direct".to_owned(), |s| format!("min {}", buffer(s, min_transfer)));
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
pub fn route(tt: &Timetable, chain: &Chain, min_transfer: TimeDelta, rejoin: &HashSet<usize>) -> String {
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
