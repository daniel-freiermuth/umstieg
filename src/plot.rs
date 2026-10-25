//! Interactive time–distance plot as a self-contained HTML file.
//!
//! x is absolute time, y a per-station "remaining distance" (goal = 0), so
//! riding goes down, waiting is horizontal, and later departures of the same
//! line are parallel lines.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use clap::ValueEnum;
use serde::Serialize;

use crate::enumerate::{self, Chain, Group, Network, Step};
use crate::fallback::Fallbacks;
use crate::page;
use crate::text;
use crate::timetable::{Timetable, meters};

/// How a station's height is computed; all are "remaining", goal = 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Metric {
    /// Fastest possible remaining ride time, ignoring waits (minutes). Stations
    /// keep one height, riding never runs flat unless moving away from the goal.
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
            Self::RemainingTime => "fastest possible remaining ride time",
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
    match metric {
        Metric::RemainingTime => return Ok(net.lower_bound.iter().map(minutes).collect()),
        Metric::FastestReal => return Ok(enumerate::fastest_journeys(tt).iter().map(minutes).collect()),
        _ => {}
    }
    let (Some(start), Some(goal)) = (tt.stations[tt.origin].position, tt.stations[tt.destination].position) else {
        bail!("--metric {metric:?} needs coordinates of origin and destination");
    };
    let km = |a, b| meters(a, b) / 1000.0;
    let total = km(start, goal);
    Ok(tt
        .stations
        .iter()
        .map(|s| {
            let p = s.position?;
            let (a, b) = (km(p, start), km(p, goal));
            Some(match metric {
                Metric::RemainingTime | Metric::FastestReal => unreachable!("handled above"),
                Metric::Hyperbolic => f64::midpoint(b - a, total),
                Metric::Axis => total - f64::mul_add(b, -b, f64::mul_add(a, a, total * total)) / (2.0 * total),
                Metric::Ratio => 100.0 * b / (a + b).max(f64::EPSILON),
            })
        })
        .collect())
}

#[derive(Serialize)]
struct Data {
    title: String,
    subtitle: String,
    /// Link back to the search form, if any.
    back: Option<String>,
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
    route: String,
    /// Indexes into `Data::pieces`.
    pieces: Vec<usize>,
    /// Fallbacks as (index into `Data::fallbacks`, arrival at the destination):
    /// shared routes, but the arrival depends on this connection's plan.
    fallbacks: Vec<(usize, String)>,
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
    used: HashSet<usize>,
    pieces: Vec<Piece>,
    index: HashMap<Piece, usize>,
}

const fn point(station: usize, t: DateTime<Utc>) -> (i64, usize) {
    (t.timestamp() / 60, station)
}

impl Builder<'_> {
    fn intern(&mut self, piece: Piece) -> usize {
        if let Some(&i) = self.index.get(&piece) {
            return i;
        }
        self.pieces.push(piece.clone());
        self.index.insert(piece, self.pieces.len() - 1);
        self.pieces.len() - 1
    }

    /// Pieces of `chain`, up to boarding ride `until` (exclusive) if given.
    /// `start`: where and from when the chain's first ride is waited for (a
    /// fallback, from the missed departure on).
    #[expect(clippy::expect_used, reason = "chains only board calls that depart and alight at calls that arrive")]
    fn pieces(&mut self, chain: &Chain, until: Option<usize>, start: Option<(usize, DateTime<Utc>)>) -> Vec<usize> {
        let tt = self.tt;
        let mut out = Vec::new();
        let mut here = start;
        let mut ride = 0;
        for step in &chain.steps {
            match *step {
                Step::Walk { footpath } => {
                    let fp = &tt.footpaths[footpath];
                    let Some((from, t)) = here else { continue };
                    let to_time = t + fp.duration;
                    let pts = vec![point(from, t), point(fp.to, to_time)];
                    let info = format!("walk {} to {}", text::duration(fp.duration), tt.stations[fp.to].name);
                    out.push(self.intern(Piece { kind: "walk", info, warn: false, trip: None, pts }));
                    self.used.insert(fp.to);
                    here = Some((fp.to, to_time));
                }
                Step::Ride { trip, board, alight } => {
                    let t = &tt.trips[trip];
                    let (b, a) = (&t.calls[board], &t.calls[alight]);
                    let departure = b.departure_time().expect("boarded calls depart");
                    if let Some((_, since)) = here {
                        let wait = departure - since;
                        // A missed change (`--missed-by`) leaves before the arrival: drawn
                        // forward over the gap, red like other too-short changes.
                        let pts = vec![point(b.station, since.min(departure)), point(b.station, since.max(departure))];
                        let station = &tt.stations[b.station].name;
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
                    for (k, call) in t.calls.iter().enumerate().take(alight + 1).skip(board) {
                        if k > board {
                            pts.extend(call.arrival_time().map(|at| point(call.station, at)));
                        }
                        if k < alight {
                            pts.extend(call.departure_time().map(|at| point(call.station, at)));
                        }
                    }
                    let (bs, as_) = (&tt.stations[b.station], &tt.stations[a.station]);
                    let info = format!(
                        "{}{} · {} {} → {} {}",
                        t.label,
                        t.headsign.as_deref().map(|h| format!(" → {h}")).unwrap_or_default(),
                        text::timed(b.departure, bs.tz.as_deref()),
                        bs.name,
                        text::timed(a.arrival.or(a.departure), as_.tz.as_deref()),
                        as_.name,
                    );
                    out.push(self.intern(Piece { kind: "ride", info, warn: false, trip: Some(trip), pts }));
                    self.used.insert(a.station);
                    here = Some((a.station, a.arrival_time().expect("alighted calls arrive")));
                    ride += 1;
                }
            }
        }
        out
    }
}

pub struct Plot<'a> {
    pub title: String,
    pub subtitle: String,
    pub groups: &'a [Group],
    pub metric: Metric,
    pub min_transfer: TimeDelta,
    /// Link back to the search form that produced the plot (`umstieg serve`).
    pub back: Option<String>,
}

/// The plot of all connections in `plot.groups`, with fallbacks per change, as
/// a self-contained HTML page.
pub fn render(net: &Network<'_>, fallbacks: &mut Fallbacks<'_>, plot: &Plot<'_>) -> Result<String> {
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
        used: HashSet::from([tt.origin, tt.destination]),
        pieces: Vec::new(),
        index: HashMap::new(),
    };
    let destination_tz = tt.stations[tt.destination].tz.as_deref();

    let mut fallback_out: Vec<FallbackOut> = Vec::new();
    let mut fallback_index: HashMap<(usize, DateTime<Utc>, String), usize> = HashMap::new();
    let mut chains = Vec::new();
    for (g, group) in plot.groups.iter().enumerate() {
        for chain in &group.chains {
            let mut refs = Vec::new();
            for (change, transfer) in chain.transfers(tt).iter().enumerate() {
                for fallback in fallbacks.for_change(chain, change) {
                    let key = (transfer.station, transfer.departure, fallback.line.clone());
                    let i = if let Some(&i) = fallback_index.get(&key) {
                        i
                    } else {
                        fallback_out.push(FallbackOut {
                            at: tt.stations[transfer.station].name.clone(),
                            pieces: builder.pieces(
                                &fallback.chain,
                                fallback.rejoins_at,
                                Some((transfer.station, transfer.departure)),
                            ),
                            line: fallback.line,
                        });
                        fallback_index.insert(key, fallback_out.len() - 1);
                        fallback_out.len() - 1
                    };
                    let late = (fallback.lands - chain.arrival(tt)).max(TimeDelta::zero());
                    let arrive = format!("{} (+{})", text::clock(fallback.lands, destination_tz), text::duration(late));
                    refs.push((i, arrive));
                }
            }
            chains.push(ChainOut {
                group: g,
                header: text::header(tt, chain, plot.min_transfer),
                route: text::route(tt, chain, plot.min_transfer, &HashSet::new()),
                pieces: builder.pieces(chain, None, None),
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
        tz: tt.stations[tt.origin].tz.clone().unwrap_or_else(|| "Europe/Berlin".to_owned()),
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
            .map(|g| GroupOut {
                label: format!("{} … {}", tt.trips[g.first].label, tt.trips[g.last].label),
                count: g.chains.len(),
            })
            .collect(),
        pieces: builder.pieces,
        fallbacks: fallback_out,
        chains,
    };
    page::PLOT.render(Some(&data))
}

#[cfg(test)]
#[path = "plot_tests.rs"]
mod tests;
