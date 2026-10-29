//! Interactive time–distance plot as a self-contained HTML file.
//!
//! x is absolute time, y a per-station "remaining distance" (goal = 0), so
//! riding goes down, waiting is horizontal, and later departures of the same
//! line are parallel lines.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;
use clap::ValueEnum;
use serde::Serialize;

use crate::checked;
use crate::enumerate::{self, Chain, Group, Network, Step};
use crate::fallback::Fallbacks;
use crate::page;
use crate::text;
use crate::timetable::{Timetable, meters};

/// How a station's height is computed; all are "remaining", goal = 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Metric {
    /// Fastest possible remaining ride time, ignoring waits, every stretch
    /// ridden either way (minutes). No ride or walk changes height by more
    /// than its duration: at most 45°, the fastest way down at exactly 45°.
    RemainingTime,
    /// Duration of the fastest real journey from there, waits included, at any
    /// departure time in the timetable (minutes). Each height is a connection
    /// one can look up; riding can run flat or rise where connections are poor.
    FastestReal,
    /// (distance to goal − distance from start + D) / 2 (km): hyperbolas with
    /// start and goal as foci.
    Hyperbolic,
    /// Distance left along the start–goal axis (km), from `d_start²` − `d_goal²`:
    /// straight lines perpendicular to the axis.
    Axis,
    /// `d_goal` / (`d_start` + `d_goal`) in percent: Apollonius circles.
    Ratio,
}

impl Metric {
    const fn unit(self) -> &'static str {
        match self {
            Self::RemainingTime | Self::FastestReal => "min",
            Self::Hyperbolic | Self::Axis => "km",
            Self::Ratio => "%",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::RemainingTime => "fastest possible remaining ride time, either direction",
            Self::FastestReal => "fastest real remaining journey, any departure",
            Self::Hyperbolic => "(distance to goal − distance from start + D) / 2",
            Self::Axis => "distance left along the start–goal axis",
            Self::Ratio => "distance to goal / (distance from start + distance to goal)",
        }
    }
}

/// Height per station, `None` where it can't be computed (no coordinates, or
/// the goal is unreachable from there).
fn heights(net: &Network<'_>, metric: Metric) -> Result<Vec<Option<f64>>> {
    let tt = net.tt;
    let minutes = |d: &Option<TimeDelta>| d.map(|d| d.as_seconds_f64() / 60.0);
    let geometric: fn(f64, f64, f64) -> f64 = match metric {
        Metric::RemainingTime => {
            return Ok(remaining_ride_minutes(tt)?.into_iter().map(|m| m.map(f64::from)).collect());
        }
        Metric::FastestReal => return Ok(enumerate::fastest_journeys(tt)?.iter().map(minutes).collect()),
        Metric::Hyperbolic => |a, b, total| f64::midpoint(b - a, total),
        Metric::Axis => |a, b, total| total - f64::mul_add(b, -b, f64::mul_add(a, a, total * total)) / (2.0 * total),
        Metric::Ratio => |a, b, _| 100.0 * b / (a + b).max(f64::EPSILON),
    };
    let (Some(start), Some(goal)) = (tt.station(tt.origin)?.position, tt.station(tt.destination)?.position) else {
        bail!("--metric {metric:?} needs coordinates of origin and destination");
    };
    let km = |a, b| meters(a, b) / 1000.0;
    let total = km(start, goal);
    Ok(tt
        .stations
        .iter()
        .map(|s| {
            let p = s.position?;
            Some(geometric(km(p, start), km(p, goal), total))
        })
        .collect())
}

/// Shortest ride and walk time to the goal with every stretch usable both
/// ways, in whole minutes. The search's lower bound counts stretches only in
/// the direction the train runs, and the timetable mostly holds trains towards
/// the goal: a station on a detour got the whole loop as its height, a step up
/// far steeper than the ride to it. Trains back almost always exist.
///
/// Edges are what the plot draws, so that no drawn piece rises or falls more
/// than it lasts: minutes as drawn (whole minutes of the timestamps), the
/// shorter of planned and live time, cancelled stops included, walks rounded
/// down. Heights are i32 minutes: a lower bound beyond ~4000 years saturates.
fn remaining_ride_minutes(tt: &Timetable) -> Result<Vec<Option<i32>>> {
    let minute = |t: DateTime<Utc>| t.timestamp().div_euclid(60);
    let mut edges: Vec<Vec<(usize, i64)>> = vec![Vec::new(); tt.stations.len()];
    let mut connect = |a: usize, b: usize, w: i64| -> Result<()> {
        if a != b {
            edges.get_mut(a).with_context(|| format!("no station {a} in the timetable"))?.push((b, w));
            edges.get_mut(b).with_context(|| format!("no station {b} in the timetable"))?.push((a, w));
        }
        Ok(())
    };
    for trip in &tt.trips {
        let mut previous: Option<(usize, i64, i64)> = None;
        for call in &trip.calls {
            let at = |t: crate::timetable::Time| (minute(t.planned), minute(t.actual));
            if let (Some((from, planned, live)), Some((p, l))) = (previous, call.arrival.or(call.departure).map(at)) {
                connect(from, call.station, p.saturating_sub(planned).min(l.saturating_sub(live)).max(0))?;
            }
            if let Some((p, l)) = call.departure.or(call.arrival).map(at) {
                previous = Some((call.station, p, l));
            }
        }
    }
    for fp in &tt.footpaths {
        connect(fp.from, fp.to, fp.duration.num_minutes().max(0))?;
    }
    let best = enumerate::shortest_paths(&edges, tt.destination, 0_i64, i64::saturating_add)?;
    Ok(best.into_iter().map(|b| b.map(|m| i32::try_from(m).unwrap_or(i32::MAX))).collect())
}

#[derive(Serialize)]
struct Data {
    title: String,
    subtitle: String,
    /// Link back to the search form, if any.
    back: Option<String>,
    /// The time zone every time is shown in: "Europe/Berlin".
    tz: String,
    /// Every metric computable here; the page switches between them.
    metrics: Vec<MetricOut>,
    /// Index into `metrics` shown first.
    metric: usize,
    /// Stations with a row: where connections board, alight or walk.
    stations: Vec<usize>,
    /// Names of all stations, indexed by the station ids in piece points.
    names: Vec<String>,
    /// "ICE 378 → Berlin Hbf" per trip, indexed by `Piece::trip`.
    trips: Vec<String>,
    groups: Vec<GroupOut>,
    /// Drawable stretches, shared by all connections using them.
    pieces: Vec<Piece>,
    fallbacks: Vec<FallbackOut>,
    chains: Vec<ChainOut>,
    /// The connections' details in the page itself (a file has no server to
    /// ask); `null` when the page fetches them.
    details: Option<Vec<Detail>>,
    /// Where to ask for details (`?ids=` comma-separated), when not in the page.
    details_url: Option<String>,
}

#[derive(Serialize)]
struct MetricOut {
    /// The `--metric` value.
    id: String,
    unit: &'static str,
    description: &'static str,
    /// Height per station, `null` where it can't be computed.
    heights: Vec<Option<f64>>,
}

#[derive(Serialize)]
struct GroupOut {
    label: String,
    count: usize,
}

#[derive(Serialize)]
struct ChainOut {
    group: usize,
    header: String,
    /// Indexes into `Data::pieces`.
    pieces: Vec<usize>,
}

/// What a connection's selection shows: by far the bulk of the plot's data
/// (a night search lists 140 000 connections), so `umstieg serve` hands it out
/// per connection when asked (`/job/{id}/chains`) instead of in the page.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub route: String,
    /// Fallbacks as (index into `Data::fallbacks`, arrival at the destination):
    /// shared routes, but the arrival depends on this connection's plan.
    pub fallbacks: Vec<(usize, String)>,
}

/// The plot page and the details of its connections, by connection index.
pub struct Rendered {
    pub html: String,
    pub details: Vec<Detail>,
}

#[derive(Serialize)]
struct FallbackOut {
    /// Station where the change is missed.
    at: String,
    line: String,
    pieces: Vec<usize>,
}

/// A ride, a wait or a walk, as (unix minutes, station) points; heights come
/// from the metric chosen in the page.
#[derive(Serialize, Clone, PartialEq, Eq, Hash)]
struct Piece {
    kind: &'static str,
    info: String,
    warn: bool,
    /// Index into `Data::trips` for rides: hovering works per train and stop-to-stop
    /// stretch, so overlapping rides of the same train count as one line.
    trip: Option<usize>,
    pts: Vec<(i64, usize)>,
}

struct Builder<'a> {
    tt: &'a Timetable,
    min_transfer: TimeDelta,
    zone: Tz,
    used: HashSet<usize>,
    pieces: Vec<Piece>,
    index: HashMap<Piece, usize>,
}

const fn point(station: usize, t: DateTime<Utc>) -> (i64, usize) {
    (t.timestamp().div_euclid(60), station)
}

impl Builder<'_> {
    fn intern(&mut self, piece: Piece) -> usize {
        if let Some(&i) = self.index.get(&piece) {
            return i;
        }
        let i = self.pieces.len();
        self.pieces.push(piece.clone());
        self.index.insert(piece, i);
        i
    }

    /// Pieces of `chain`, up to boarding ride `until` (exclusive) if given.
    /// `start`: where and from when the chain's first ride is waited for (a
    /// fallback, from the missed departure on, or from the arrival if later).
    fn pieces(
        &mut self,
        chain: &Chain,
        until: Option<usize>,
        start: Option<(usize, DateTime<Utc>)>,
    ) -> Result<Vec<usize>> {
        let tt = self.tt;
        let mut out = Vec::new();
        let mut here = start;
        let mut ride = 0_usize;
        for step in &chain.steps {
            match *step {
                Step::Walk { footpath } => {
                    let fp = tt.footpath(footpath)?;
                    let Some((from, t)) = here else { continue };
                    let to_time = checked::later(t, fp.duration, "walk")?;
                    let pts = vec![point(from, t), point(fp.to, to_time)];
                    let info = format!("walk {} to {}", text::duration(fp.duration), tt.station(fp.to)?.name);
                    out.push(self.intern(Piece { kind: "walk", info, warn: false, trip: None, pts }));
                    self.used.insert(fp.to);
                    here = Some((fp.to, to_time));
                }
                Step::Ride { trip, board, alight } => {
                    let t = tt.trip(trip)?;
                    let (b, a) = (tt.call(trip, board)?, tt.call(trip, alight)?);
                    let departure =
                        b.departure_time().with_context(|| format!("{} boarded where it doesn't depart", t.label))?;
                    if let Some((_, since)) = here {
                        let wait = departure.signed_duration_since(since);
                        // A missed change (`--missed-by`) leaves before the arrival: drawn
                        // forward over the gap, red like other too-short changes.
                        let pts = vec![point(b.station, since.min(departure)), point(b.station, since.max(departure))];
                        let station = &tt.station(b.station)?.name;
                        // Waiting after a missed train is no change that could be too short.
                        let (info, warn) = if ride == 0 && start.is_some() {
                            (format!("{} at {station} after the missed train", text::duration(wait)), false)
                        } else {
                            (
                                format!("{} at {station}", text::buffer(wait, self.min_transfer)),
                                wait < self.min_transfer,
                            )
                        };
                        out.push(self.intern(Piece { kind: "wait", info, warn, trip: None, pts }));
                    }
                    self.used.insert(b.station);
                    if until == Some(ride) {
                        break;
                    }
                    let mut pts = Vec::new();
                    for (k, call) in t.calls.iter().enumerate().take(alight.saturating_add(1)).skip(board) {
                        if k > board {
                            pts.extend(call.arrival_time().map(|at| point(call.station, at)));
                        }
                        if k < alight {
                            pts.extend(call.departure_time().map(|at| point(call.station, at)));
                        }
                    }
                    let (bs, as_) = (tt.station(b.station)?, tt.station(a.station)?);
                    let info = format!(
                        "{}{} · {} {} → {} {}",
                        t.label,
                        t.headsign.as_deref().map(|h| format!(" → {h}")).unwrap_or_default(),
                        text::timed(b.departure, self.zone),
                        bs.name,
                        text::timed(a.arrival.or(a.departure), self.zone),
                        as_.name,
                    );
                    out.push(self.intern(Piece { kind: "ride", info, warn: false, trip: Some(trip), pts }));
                    self.used.insert(a.station);
                    let arrival =
                        a.arrival_time().with_context(|| format!("{} alighted where it doesn't arrive", t.label))?;
                    here = Some((a.station, arrival));
                    ride = ride.saturating_add(1);
                }
            }
        }
        Ok(out)
    }
}

pub struct Plot<'a> {
    pub title: String,
    pub subtitle: String,
    pub groups: &'a [Group],
    pub metric: Metric,
    pub min_transfer: TimeDelta,
    /// Where every time is shown in.
    pub zone: Tz,
    /// Link back to the search form that produced the plot (`umstieg serve`).
    pub back: Option<String>,
    /// Where the page fetches its connections' details; `None` puts them into the page.
    pub details_url: Option<String>,
    /// Told (connections drawn, of how many) about every `REPORT_EVERY`.
    pub progress: &'a dyn Fn(usize, usize),
}

/// The plot of all connections in `plot.groups`, with fallbacks per change, as
/// an HTML page, and the connections' details.
#[expect(clippy::too_many_lines, reason = "the page's data, part after part")]
pub fn render(net: &Network<'_>, fallbacks: &mut Fallbacks<'_>, plot: &Plot<'_>) -> Result<Rendered> {
    let tt = net.tt;
    let mut metrics = Vec::new();
    let mut shown = 0;
    for &m in Metric::value_variants() {
        let heights = match heights(net, m) {
            Ok(h) => h,
            // Only the requested metric must work; others are offered if they do.
            Err(e) if m == plot.metric => return Err(e),
            Err(_) => continue,
        };
        if m == plot.metric {
            shown = metrics.len();
        }
        let id = m.to_possible_value().map(|v| v.get_name().to_owned()).unwrap_or_default();
        let heights = heights.into_iter().map(|h| h.map(|h| (h * 100.0).round() / 100.0)).collect();
        metrics.push(MetricOut { id, unit: m.unit(), description: m.description(), heights });
    }
    let mut builder = Builder {
        tt,
        min_transfer: plot.min_transfer,
        zone: plot.zone,
        used: HashSet::from([tt.origin, tt.destination]),
        pieces: Vec::new(),
        index: HashMap::new(),
    };

    let mut fallback_out: Vec<FallbackOut> = Vec::new();
    let mut fallback_index: HashMap<(usize, DateTime<Utc>, String), usize> = HashMap::new();
    let mut chains = Vec::new();
    let mut details = Vec::new();
    let total: usize = plot.groups.iter().map(|g| g.chains.len()).sum();
    let mut report_at = enumerate::next_report();
    for (g, group) in plot.groups.iter().enumerate() {
        for chain in &group.chains {
            if Instant::now() >= report_at {
                report_at = enumerate::next_report();
                (plot.progress)(chains.len(), total);
            }
            let arrival = chain.arrival(tt)?;
            let mut refs = Vec::new();
            for (change, transfer) in chain.transfers(tt)?.iter().enumerate() {
                // A missed change (`--missed-by`): waited from the arrival, as
                // fallbacks are searched from then on.
                let since = transfer.departure.max(transfer.ready());
                for fallback in fallbacks.for_change(chain, change)? {
                    let key = (transfer.station, since, fallback.line.clone());
                    let i = if let Some(&i) = fallback_index.get(&key) {
                        i
                    } else {
                        let i = fallback_out.len();
                        fallback_out.push(FallbackOut {
                            at: tt.station(transfer.station)?.name.clone(),
                            pieces: builder.pieces(
                                &fallback.chain,
                                fallback.rejoins_at,
                                Some((transfer.station, since)),
                            )?,
                            line: fallback.line,
                        });
                        fallback_index.insert(key, i);
                        i
                    };
                    let late = fallback.lands.signed_duration_since(arrival).max(TimeDelta::zero());
                    let arrive = format!("{} (+{})", text::clock(fallback.lands, plot.zone), text::duration(late));
                    refs.push((i, arrive));
                }
            }
            chains.push(ChainOut {
                group: g,
                header: text::header(tt, chain, plot.min_transfer, plot.zone)?,
                pieces: builder.pieces(chain, None, None)?,
            });
            details.push(Detail {
                route: text::route(tt, chain, plot.min_transfer, &HashSet::new(), plot.zone)?,
                fallbacks: refs,
            });
        }
    }

    let mut stations: Vec<usize> = builder.used.into_iter().collect();
    stations.sort_unstable();

    let data = Data {
        title: plot.title.clone(),
        subtitle: plot.subtitle.clone(),
        back: plot.back.clone(),
        tz: plot.zone.name().to_owned(),
        metrics,
        metric: shown,
        stations,
        names: tt.stations.iter().map(|s| s.name.clone()).collect(),
        trips: tt
            .trips
            .iter()
            .map(|t| format!("{}{}", t.label, t.headsign.as_deref().map(|h| format!(" → {h}")).unwrap_or_default()))
            .collect(),
        groups: plot
            .groups
            .iter()
            .map(|g| {
                Ok(GroupOut {
                    label: format!("{} … {}", tt.trip(g.first)?.label, tt.trip(g.last)?.label),
                    count: g.chains.len(),
                })
            })
            .collect::<Result<_>>()?,
        pieces: builder.pieces,
        fallbacks: fallback_out,
        chains,
        details: plot.details_url.is_none().then(|| details.clone()),
        details_url: plot.details_url.clone(),
    };
    Ok(Rendered { html: page::PLOT.render(Some(&data))?, details })
}

#[cfg(test)]
#[path = "plot_tests.rs"]
mod tests;
