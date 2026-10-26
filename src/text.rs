//! Text formatting shared by the terminal output and the HTML plot.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;

use crate::enumerate::{Chain, Step};
use crate::timetable::{Time, Timetable};

/// "HH:MM" in `zone`, with the zone's abbreviation: "06:39 CEST".
pub fn clock(t: DateTime<Utc>, zone: Tz) -> String {
    t.with_timezone(&zone).format("%H:%M %Z").to_string()
}

pub fn duration(d: TimeDelta) -> String {
    let m = d.num_minutes();
    let (hours, rest) = (m.checked_div(60).unwrap_or_default(), m.checked_rem(60).unwrap_or_default());
    if m.abs() >= 60 { format!("{hours}h{:02}", rest.abs()) } else { format!("{m}m") }
}

/// Time plus live delay, e.g. "11:51 CET" or "11:55 CET (+4)".
pub fn timed(t: Option<Time>, zone: Tz) -> String {
    let Some(t) = t else { return "--:--".to_owned() };
    let delay = t.delay().num_minutes();
    if delay != 0 { format!("{} ({delay:+})", clock(t.actual, zone)) } else { clock(t.actual, zone) }
}

/// "12m", "3m⚠" below the minimum transfer, "missed 2m✗" for a change that
/// live data says is gone (listed with `--missed-by`: delays may still shift).
pub fn buffer(slack: TimeDelta, min_transfer: TimeDelta) -> String {
    if slack < TimeDelta::zero() {
        return format!("missed {}✗", duration(slack.abs()));
    }
    let warn = if slack < min_transfer { "⚠" } else { "" };
    format!("{}{warn}", duration(slack))
}

/// "06:39 CEST → 22:05 CEST  15h26  3×  min 47m"
pub fn header(tt: &Timetable, chain: &Chain, min_transfer: TimeDelta, zone: Tz) -> Result<String> {
    let departure = chain.departure(tt)?;
    let arrival = chain.arrival(tt)?;
    let min = chain.min_slack(tt)?.map_or_else(|| "direct".to_owned(), |s| format!("min {}", buffer(s, min_transfer)));
    Ok(format!(
        "{} → {}  {}  {}×  {}",
        clock(departure, zone),
        clock(arrival, zone),
        duration(arrival.signed_duration_since(departure)),
        chain.changes(),
        min,
    ))
}

/// Trains, change stations and buffers on one line, ending with a walk to the
/// destination if any. Stops at the first train in `rejoin`, marking it as
/// back on the original plan.
pub fn route(
    tt: &Timetable,
    chain: &Chain,
    min_transfer: TimeDelta,
    rejoin: &HashSet<usize>,
    zone: Tz,
) -> Result<String> {
    let transfers = chain.transfers(tt)?;
    let mut parts = Vec::new();
    for (i, (trip, board, alight)) in chain.rides().enumerate() {
        // The change before this ride: none before the first.
        let transfer = match i.checked_sub(1) {
            Some(j) => Some(transfers.get(j).with_context(|| format!("no change before ride {i}"))?),
            None => None,
        };
        let t = tt.trip(trip)?;
        let (b, a) = (tt.call(trip, board)?, tt.call(trip, alight)?);
        let (bs, as_) = (tt.station(b.station)?, tt.station(a.station)?);
        let mut from = String::new();
        if let Some(tr) = transfer {
            let walk = if tr.walk > TimeDelta::zero() { format!(" walk {}", duration(tr.walk)) } else { String::new() };
            parts.push(format!("‖{}{walk}‖", buffer(tr.slack, min_transfer)));
            if bs.name != tt.station(tr.station)?.name {
                from = format!(" {}", bs.name);
            }
        }
        if rejoin.contains(&trip) {
            parts.push(format!("{}{from} {} (back on plan)", timed(b.departure, zone), t.label));
            return Ok(parts.join(" "));
        }
        parts.push(format!(
            "{}{from} {} → {} {}",
            timed(b.departure, zone),
            t.label,
            timed(a.arrival.or(a.departure), zone),
            as_.name,
        ));
    }
    let after_last_ride =
        chain.steps.iter().rposition(|s| matches!(s, Step::Ride { .. })).map_or(0, |i| i.saturating_add(1));
    for step in chain.steps.iter().skip(after_last_ride) {
        if let Step::Walk { footpath } = *step {
            let fp = tt.footpath(footpath)?;
            parts.push(format!("walk {} to {}", duration(fp.duration), tt.station(fp.to)?.name));
        }
    }
    Ok(parts.join(" "))
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
