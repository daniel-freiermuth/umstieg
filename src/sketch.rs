//! What a running search has seen so far, for `umstieg serve`'s progress page
//! to draw behind it: the ends, the stations asked about, the trains of every
//! answer, then the timetable. Off (the command line) it costs nothing.

use std::sync::Arc;

use jiff::Timestamp;
use serde::Serialize;

use crate::db::{self, Visit};
use crate::motis::{self, Itinerary, Leg};
use crate::timetable::Timetable;
use crate::trafikverket;

/// A named place where it lies.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Place {
    pub name: String,
    /// (lat, lon), rounded to 5 decimals.
    pub at: (f64, f64),
}

/// A stop of a train as an answer has it: lat, lon, arrival, departure.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stop(pub f64, pub f64, pub Option<i64>, pub Option<i64>);

/// A train an asked station's answer lists: train id, arrival, departure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Seen(pub String, pub Option<i64>, pub Option<i64>);

/// A timetable station: where it lies and the fastest ride left from there.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Station {
    pub name: String,
    pub at: Option<(f64, f64)>,
    /// Fastest remaining ride minutes to the goal; `None` where unreachable.
    pub height: Option<i32>,
}

/// A timetable train's stop: station index, arrival, departure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Call(pub usize, pub Option<i64>, pub Option<i64>);

/// One thing drawn. Times are Unix seconds, live where known, else planned.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    /// The search's ends and departure window.
    Ends { from: Place, to: Place, depart_from: i64, depart_until: i64 },
    /// A station asked about, the time range asked for, and the trains listed there.
    Asked { place: Place, span: Option<(i64, i64)>, seen: Vec<Seen> },
    /// A train's stops as one answer has them.
    Train { id: String, stops: Vec<Stop> },
    /// The built timetable, replacing every train so far.
    Timetable { stations: Vec<Station>, trains: Vec<Vec<Call>> },
    /// Timetable train `trip` asked about.
    AskedTrain { trip: usize },
}

/// Where a search's drawing goes; off by default.
#[derive(Clone, Default)]
pub struct Sketch(Option<Arc<dyn Fn(Item) + Send + Sync>>);

impl Sketch {
    /// A sketch handing every item to `f`.
    pub fn new(f: impl Fn(Item) + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(f)))
    }

    /// Draws `item`, built only when the sketch is on.
    pub fn draw(&self, item: impl FnOnce() -> Item) {
        if let Some(f) = &self.0 {
            f(item());
        }
    }

    /// Draws every one of `items`, built only when the sketch is on.
    pub fn draw_all<I: IntoIterator<Item = Item>>(&self, items: impl FnOnce() -> I) {
        if let Some(f) = &self.0 {
            items().into_iter().for_each(|item| f(item));
        }
    }
}

/// `x` to 5 decimals (about a metre).
fn round(x: f64) -> f64 {
    (x * 100_000.0).round() / 100_000.0
}

fn rounded((lat, lon): (f64, f64)) -> (f64, f64) {
    (round(lat), round(lon))
}

fn place(name: &str, at: Option<(f64, f64)>) -> Option<Place> {
    Some(Place { name: name.to_owned(), at: rounded(at?) })
}

fn secs(t: Option<Timestamp>) -> Option<i64> {
    t.map(Timestamp::as_second)
}

/// The search's ends, if both have positions.
pub fn ends(from: &motis::Stop, to: &motis::Stop, depart_from: Timestamp, depart_until: Timestamp) -> Option<Item> {
    Some(Item::Ends {
        from: place(&from.name, from.position)?,
        to: place(&to.name, to.position)?,
        depart_from: depart_from.as_second(),
        depart_until: depart_until.as_second(),
    })
}

/// A transit leg's train: its stops with positions. `None` for walks.
pub fn train(leg: &Leg) -> Option<Item> {
    let id = leg.trip_id.clone()?;
    let stops = std::iter::once(&leg.from)
        .chain(&leg.intermediate_stops)
        .chain(std::iter::once(&leg.to))
        .filter_map(|p| {
            let (lat, lon) = rounded(p.lat.zip(p.lon)?);
            Some(Stop(lat, lon, secs(p.arrival.or(p.scheduled_arrival)), secs(p.departure.or(p.scheduled_departure))))
        })
        .collect();
    Some(Item::Train { id, stops })
}

/// The trains of every transit leg of `itineraries`.
pub fn itineraries(itineraries: &[Itinerary]) -> Vec<Item> {
    itineraries.iter().flat_map(|i| &i.legs).filter_map(train).collect()
}

/// Departures asked for at `center` (else where the first train boards) from
/// `from` until `until`, and their trains.
pub fn onward(center: Option<(f64, f64)>, from: Timestamp, until: Timestamp, legs: &[Leg]) -> Vec<Item> {
    let first = legs.first().map(|l| &l.from);
    let name = first.map_or("", |p| p.name.as_str());
    let at = center.or_else(|| first.and_then(|p| p.lat.zip(p.lon)));
    let asked = place(name, at).map(|place| Item::Asked {
        place,
        span: Some((from.as_second(), until.as_second())),
        seen: Vec::new(),
    });
    asked.into_iter().chain(legs.iter().filter_map(train)).collect()
}

/// An hour of a DB board at `station`, if it has a position.
pub fn board_hour(station: &crate::timetable::Station, hour: Timestamp, visits: &[Visit]) -> Option<Item> {
    let actual = |e: &Option<db::Event>| secs(e.as_ref().and_then(|e| e.changed.or(e.planned)));
    Some(Item::Asked {
        place: place(&station.name, station.position)?,
        span: Some((hour.as_second(), hour.as_second().saturating_add(3600))),
        seen: visits.iter().map(|v| Seen(format!("db:{}", v.trip), actual(&v.arrival), actual(&v.departure))).collect(),
    })
}

/// A Trafikverket run's id, as in `asked` and `train` items.
fn run_id(key: &trafikverket::RunKey) -> String {
    format!("tv:{}/{}", key.ident, key.date)
}

/// A Trafikverket board at `station`, if it has a position.
pub fn tv_board(
    station: &trafikverket::Station,
    (from, until): (Timestamp, Timestamp),
    seen: &[trafikverket::Seen],
) -> Option<Item> {
    Some(Item::Asked {
        place: place(&station.name, station.position)?,
        span: Some((from.as_second(), until.as_second())),
        seen: seen
            .iter()
            .map(|s| Seen(run_id(&s.run), Some(s.planned.as_second()), Some(s.planned.as_second())))
            .collect(),
    })
}

/// A Trafikverket run's train: its stops at stations `position` knows.
pub fn tv_run(run: &trafikverket::Run, position: impl Fn(&str) -> Option<(f64, f64)>) -> Item {
    let time = |m: Option<trafikverket::Moment>| m.map(|m| m.live.unwrap_or(m.planned).as_second());
    let stops = run
        .stops
        .iter()
        .filter_map(|s| {
            let (lat, lon) = rounded(position(&s.signature)?);
            Some(Stop(lat, lon, time(s.arrival), time(s.departure)))
        })
        .collect();
    Item::Train { id: run_id(&run.key), stops }
}

/// The timetable, with `heights` by station index (missing ones `None`).
pub fn timetable(tt: &Timetable, heights: &[Option<i32>]) -> Item {
    let stations = tt
        .stations
        .iter()
        .enumerate()
        .map(|(i, s)| Station {
            name: s.name.clone(),
            at: s.position.map(rounded),
            height: heights.get(i).copied().flatten(),
        })
        .collect();
    let actual = |t: Option<crate::timetable::Time>| t.map(|t| t.actual.as_second());
    let trains = tt
        .trips
        .iter()
        .map(|trip| trip.calls.iter().map(|c| Call(c.station.0, actual(c.arrival), actual(c.departure))).collect())
        .collect();
    Item::Timetable { stations, trains }
}

#[cfg(test)]
#[path = "sketch_tests.rs"]
mod tests;
