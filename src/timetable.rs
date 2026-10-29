//! A small timetable for the corridor of one search: trains from MOTIS rough
//! routes, overridden and extended by DB station boards in Germany and
//! Trafikverket's train runs in Sweden where available.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use futures::future::join_all;

use crate::checked;
use crate::db::{self, Coverage, Db, Event, Visit};
use crate::motis::{Itinerary, Leg, Motis, Place, Stop};
use crate::trafikverket::{self, Trafikverket};

/// Same train number within this time difference at a shared station = same train.
const MERGE_TOLERANCE: TimeDelta = TimeDelta::minutes(60);
/// Without a matching number, two shared stations within this difference = same train.
const SAME_TIME: TimeDelta = TimeDelta::minutes(2);
/// A train makes up at most 1/`MAX_CATCH_UP` of a stretch's planned time.
const MAX_CATCH_UP: i64 = 4;
/// Boards are fetched this far around the times MOTIS has for a station.
const BOARD_MARGIN: TimeDelta = TimeDelta::minutes(30);
const MAX_HOURS_PER_STATION: usize = 10;
/// Cap on stations added because DB trains call there between corridor stations.
const MAX_DISCOVERED: usize = 20;
/// Stops of different feeds closer than this are one station.
const SAME_STATION_METERS: f64 = 300.0;
/// Walk between nearby stations without a MOTIS footpath: `SAME_STATION_METERS`
/// at walking pace (1 m/s) is at most this long.
const NEARBY_WALK: TimeDelta = TimeDelta::minutes(5);
/// DB boards cover German stations; elsewhere they are empty.
const DB_TIME_ZONE: &str = "Europe/Berlin";
/// A stop of another feed this close to a Trafikverket station is that station.
const TV_SAME_STATION_METERS: f64 = 600.0;
/// Stations Trafikverket's runs share (junctions such as Alvesta) whose boards
/// are fetched too, at most.
const MAX_TV_DISCOVERED: usize = 12;
/// Termini of Trafikverket's runs whose boards are fetched too, at most.
const MAX_TV_TERMINI: usize = 6;
/// A station trains reach from this many directions is a junction.
const JUNCTION_DIRECTIONS: usize = 3;
/// Neighbours whose bearings differ by at most this lie in one direction.
const SAME_DIRECTION_DEGREES: f64 = 60.0;
/// Discovered stations further off the line between origin and destination
/// than this detour (via it / straight) are left out.
const MAX_DETOUR: f64 = 1.5;
/// At a discovered station, onward trains are looked for this long after the
/// last train of a run found there.
const TV_ONWARD: TimeDelta = TimeDelta::hours(2);
/// Countries whose stops Trafikverket's runs keep; German stops are DB's.
const TV_COUNTRIES: [&str; 3] = ["SE", "DK", "NO"];
/// Categories of long-distance trains, for feeds that give no product class or
/// a generic one: EFA files ICE 273 under "Zug", DB gives `FlixTrain` none.
const LONG_DISTANCE: [&str; 11] = ["ICE", "IC", "EC", "ECE", "EN", "NJ", "RJ", "RJX", "TGV", "FLX", "ICD"];

/// Whether `category` ("ICE", "FLX") is a long-distance train's.
pub fn is_long_distance(category: &str) -> bool {
    LONG_DISTANCE.contains(&category)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    pub planned: DateTime<Utc>,
    pub actual: DateTime<Utc>,
}

impl Time {
    fn new(planned: Option<DateTime<Utc>>, actual: Option<DateTime<Utc>>) -> Option<Self> {
        let planned = planned.or(actual)?;
        Some(Self { planned, actual: actual.unwrap_or(planned) })
    }

    fn from_event(e: &Event) -> Option<Self> {
        Self::new(e.planned, e.changed)
    }

    pub fn delay(&self) -> TimeDelta {
        self.actual.signed_duration_since(self.planned)
    }
}

#[derive(Debug, Clone)]
pub struct Call {
    pub station: usize,
    pub arrival: Option<Time>,
    pub departure: Option<Time>,
    pub platform: Option<String>,
    pub cancelled: bool,
    /// Times from a station board feed (DB, Trafikverket): live data is
    /// authoritative there, elsewhere the last known delay is carried forward.
    pub from_board: bool,
}

impl Call {
    pub fn arrival_time(&self) -> Option<DateTime<Utc>> {
        self.arrival.or(self.departure).map(|t| t.actual)
    }

    pub fn departure_time(&self) -> Option<DateTime<Utc>> {
        self.departure.map(|t| t.actual)
    }

    fn planned(&self) -> Option<DateTime<Utc>> {
        self.arrival.or(self.departure).map(|t| t.planned)
    }
}

#[derive(Debug, Clone)]
pub struct Trip {
    pub label: String,
    /// Train numbers; a run can change number on the way.
    pub numbers: Vec<u32>,
    pub class: Option<char>,
    pub headsign: Option<String>,
    pub calls: Vec<Call>,
    /// Whether Transitous or EFA knows its stops: it came from a rough route
    /// or one was merged into it. Otherwise only board feeds know it.
    pub transitous: bool,
}

impl Trip {
    fn call_at(&self, station: usize) -> Option<&Call> {
        self.calls.iter().find(|c| c.station == station)
    }
}

#[derive(Debug, Clone)]
pub struct Station {
    pub name: String,
    pub eva: Option<String>,
    /// Trafikverket's station signature ("G" for Göteborg C).
    pub tv: Option<String>,
    /// Name in the board feed's data (DB's, or Trafikverket's in Sweden), as
    /// used in train routes.
    pub board_name: Option<String>,
    pub tz: Option<String>,
    pub position: Option<(f64, f64)>,
    /// Transitous (MOTIS) station id, for asking about trains there.
    pub motis_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Footpath {
    pub from: usize,
    pub to: usize,
    pub duration: TimeDelta,
}

pub struct Timetable {
    pub stations: Vec<Station>,
    pub trips: Vec<Trip>,
    pub footpaths: Vec<Footpath>,
    pub origin: usize,
    pub destination: usize,
}

impl Timetable {
    /// Station `i`; an error if the index came from somewhere else.
    pub fn station(&self, i: usize) -> anyhow::Result<&Station> {
        self.stations.get(i).ok_or_else(|| anyhow::anyhow!("no station {i} in the timetable"))
    }

    /// Trip `i`.
    pub fn trip(&self, i: usize) -> anyhow::Result<&Trip> {
        self.trips.get(i).ok_or_else(|| anyhow::anyhow!("no trip {i} in the timetable"))
    }

    /// Call `call` of trip `trip`.
    pub fn call(&self, trip: usize, call: usize) -> anyhow::Result<&Call> {
        self.trip(trip)?.calls.get(call).ok_or_else(|| anyhow::anyhow!("trip {trip} has no call {call}"))
    }

    /// Footpath `i`.
    pub fn footpath(&self, i: usize) -> anyhow::Result<&Footpath> {
        self.footpaths.get(i).ok_or_else(|| anyhow::anyhow!("no footpath {i} in the timetable"))
    }

    /// Planned times everywhere: delays are dropped, while the latest schedule
    /// (cancellations, diversions, extra trains from DB's changes) stays.
    pub fn ignore_delays(&mut self) {
        for call in self.trips.iter_mut().flat_map(|t| t.calls.iter_mut()) {
            for time in [&mut call.arrival, &mut call.departure].into_iter().flatten() {
                time.actual = time.planned;
            }
        }
    }
}

pub struct Build {
    pub timetable: Timetable,
    /// Stations added from DB train paths and Trafikverket's runs.
    pub discovered: Vec<String>,
    pub warnings: Vec<String>,
    /// Trains from Trafikverket are in the timetable (its data is credited).
    pub trafikverket: bool,
}

/// What is known about a station when it is first seen.
#[derive(Default, Clone, Copy)]
struct Sighting<'a> {
    name: &'a str,
    eva: Option<&'a str>,
    tv: Option<&'a str>,
    board_name: Option<&'a str>,
    tz: Option<&'a str>,
    position: Option<(f64, f64)>,
    motis_id: Option<&'a str>,
}

#[derive(Default)]
struct Stations {
    list: Vec<Station>,
    by_key: HashMap<String, usize>,
}

impl Stations {
    /// Interning further stations into a finished timetable's list.
    fn from_list(list: Vec<Station>) -> Self {
        let mut by_key = HashMap::new();
        for (i, s) in list.iter().enumerate() {
            by_key.insert(format!("name:{}", db::fold(&s.name)), i);
            if let Some(name) = &s.board_name {
                by_key.insert(format!("name:{}", db::fold(name)), i);
            }
            if let Some(eva) = &s.eva {
                by_key.insert(format!("eva:{eva}"), i);
            }
            if let Some(tv) = &s.tv {
                by_key.insert(format!("tv:{tv}"), i);
            }
        }
        Self { list, by_key }
    }

    /// Station `idx` is Trafikverket's `station`: its trains from there on.
    fn claim(&mut self, idx: usize, station: &trafikverket::Station) {
        let Some(st) = self.list.get_mut(idx) else { return };
        st.tv = Some(station.signature.clone());
        if st.board_name.is_none() {
            st.board_name = Some(station.name.clone());
        }
        if st.position.is_none() {
            st.position = station.position;
        }
        self.by_key.insert(format!("tv:{}", station.signature), idx);
        self.by_key.insert(format!("name:{}", db::fold(&station.name)), idx);
    }

    /// One index per physical station, unified by EVA number, Trafikverket
    /// signature, folded name or proximity. Nearby stations with different EVA
    /// numbers or signatures stay separate (S-Bahn vs. long-distance station)
    /// and get a footpath instead.
    fn intern(&mut self, s: Sighting<'_>) -> usize {
        let name_key = format!("name:{}", db::fold(s.name));
        let eva_key = s.eva.map(|e| format!("eva:{e}"));
        let tv_key = s.tv.map(|t| format!("tv:{t}"));
        let found = [&eva_key, &tv_key]
            .into_iter()
            .flatten()
            .find_map(|k| self.by_key.get(k))
            .or_else(|| self.by_key.get(&name_key))
            .copied()
            .or_else(|| {
                let here = s.position?;
                self.list.iter().position(|st| {
                    let compatible = (st.eva.is_none() || s.eva.is_none()) && (st.tv.is_none() || s.tv.is_none());
                    compatible && st.position.is_some_and(|p| meters(p, here) <= SAME_STATION_METERS)
                })
            });
        let idx = found.unwrap_or_else(|| {
            self.list.push(Station {
                name: s.name.to_owned(),
                eva: None,
                tv: None,
                board_name: None,
                tz: None,
                position: None,
                motis_id: None,
            });
            self.list.len().saturating_sub(1)
        });
        self.by_key.insert(name_key, idx);
        for k in [eva_key, tv_key].into_iter().flatten() {
            self.by_key.insert(k, idx);
        }
        // `idx` came from the list itself, so the station is always there.
        let Some(station) = self.list.get_mut(idx) else { return idx };
        if station.eva.is_none() {
            station.eva = s.eva.map(str::to_owned);
        }
        if station.tv.is_none() {
            station.tv = s.tv.map(str::to_owned);
        }
        if station.board_name.is_none() {
            station.board_name = s.board_name.map(str::to_owned);
        }
        if station.tz.is_none() {
            station.tz = s.tz.map(str::to_owned);
        }
        if station.position.is_none() {
            station.position = s.position;
        }
        if station.motis_id.is_none() {
            station.motis_id = s.motis_id.map(str::to_owned);
        }
        idx
    }
}

/// A MOTIS station, with its DB identity if known.
fn sighting<'a>(
    resolved: &'a HashMap<String, db::Station>,
    name: &'a str,
    tz: Option<&'a str>,
    position: Option<(f64, f64)>,
    motis_id: Option<&'a str>,
) -> Sighting<'a> {
    let db = resolved.get(name);
    Sighting {
        name,
        eva: db.map(|s| s.eva.as_str()),
        tv: None,
        board_name: db.map(|s| s.name.as_str()),
        tz,
        position,
        motis_id,
    }
}

/// Great-circle distance between (lat, lon) pairs.
pub fn meters(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (a.0.to_radians(), a.1.to_radians(), b.0.to_radians(), b.1.to_radians());
    let h = ((la2 - la1) / 2.0)
        .sin()
        .mul_add(((la2 - la1) / 2.0).sin(), la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2));
    2.0 * 6_371_000.0 * h.sqrt().asin()
}

/// `all_stops`: fetch DB boards for every stop of the rough routes' trains,
/// not only where they start or end. More change points (e.g. RE7 to
/// Schleswig to catch the RJ there), roughly three times the DB requests.
pub async fn build(
    motis: &Motis,
    db: Option<&Db>,
    tv: Option<&Trafikverket>,
    itineraries: &[Itinerary],
    origin: &Stop,
    destination: &Stop,
    all_stops: bool,
) -> Build {
    let mut warnings = Vec::new();

    let mut resolved: HashMap<String, db::Station> = HashMap::new();
    if let Some(db) = db {
        let places = db_places(itineraries, origin, destination);
        let lookups = places.iter().map(|(name, id)| async move { (*name, db.resolve(name, *id).await) });
        for (name, result) in join_all(lookups).await {
            match result {
                Ok(Some(station)) => {
                    resolved.insert(name.to_owned(), station);
                }
                Ok(None) => {}
                Err(e) => warnings.push(format!("DB station lookup for {name}: {e:#}")),
            }
        }
    }

    let mut stations = Stations::default();
    let origin_idx = stations.intern(sighting(&resolved, &origin.name, None, origin.position, Some(&origin.id)));
    let destination_idx =
        stations.intern(sighting(&resolved, &destination.name, None, destination.position, Some(&destination.id)));
    let rough = from_motis(itineraries, &resolved, &mut stations, (origin_idx, destination_idx), all_stops);

    let mut discovered = Vec::new();
    let mut boards = match db {
        Some(db) => fetch_boards(motis, db, &mut stations, &rough.corridor, &mut discovered, &mut warnings).await,
        None => Boards::new(),
    };
    let swedish = match tv {
        Some(tv) => {
            let ends = (origin_idx, destination_idx);
            let found = swedish(tv, &mut stations, &rough.corridor, ends, &mut discovered, &mut warnings).await;
            for (idx, coverage) in found.coverage {
                boards.entry(idx).or_insert((Vec::new(), coverage));
            }
            found.trips
        }
        None => Vec::new(),
    };
    let from_trafikverket = !swedish.is_empty();
    let trips = if db.is_none() && tv.is_none() {
        rough.trips.into_values().collect()
    } else {
        merge(&stations.list, &boards, rough.trips, swedish)
    };

    Build {
        timetable: Timetable {
            footpaths: footpaths(&stations.list, rough.footpaths),
            stations: stations.list,
            trips: finish(trips),
            origin: origin_idx,
            destination: destination_idx,
        },
        discovered,
        warnings,
        trafikverket: from_trafikverket,
    }
}

/// Every German rail station MOTIS mentions (no time zone or DB's), by name
/// with its stop id: these are looked up as DB stations.
fn db_places<'a>(
    itineraries: &'a [Itinerary],
    origin: &'a Stop,
    destination: &'a Stop,
) -> HashMap<&'a str, Option<&'a str>> {
    let mut places: HashMap<&str, Option<&str>> = HashMap::new();
    places.insert(&origin.name, Some(&origin.id));
    places.insert(&destination.name, Some(&destination.id));
    for leg in itineraries.iter().flat_map(|i| &i.legs).filter(|l| l.class().is_some()) {
        for p in leg_places(leg) {
            if p.tz.as_deref().is_none_or(|tz| tz == DB_TIME_ZONE) {
                places.insert(&p.name, p.stop_id.as_deref());
            }
        }
    }
    places
}

/// What the rough routes contribute before DB is asked.
struct Rough {
    /// Trains keyed by MOTIS trip id.
    trips: HashMap<String, Trip>,
    /// Stations of rail legs, with the time span MOTIS has there; their DB boards are fetched.
    corridor: HashMap<usize, (DateTime<Utc>, DateTime<Utc>)>,
    /// Shortest walk seen per (from, to).
    footpaths: HashMap<(usize, usize), TimeDelta>,
}

/// Shortest walk per (from, to); none from a station to itself.
fn add_walk(footpaths: &mut HashMap<(usize, usize), TimeDelta>, from: usize, to: usize, walk: TimeDelta) {
    if from != to {
        let entry = footpaths.entry((from, to)).or_insert(walk);
        *entry = (*entry).min(walk);
    }
}

/// MOTIS itineraries as trains, walks and corridor; `ends` are the
/// (origin, destination) station indices.
fn from_motis(
    itineraries: &[Itinerary],
    resolved: &HashMap<String, db::Station>,
    stations: &mut Stations,
    ends: (usize, usize),
    all_stops: bool,
) -> Rough {
    let (origin_idx, destination_idx) = ends;
    let mut rough = Rough { trips: HashMap::new(), corridor: HashMap::new(), footpaths: HashMap::new() };
    for itinerary in itineraries {
        // Walks at the ends lead from / to the queried place: coordinates
        // (no stop id) stand for our origin or destination, stops for a hub.
        let end_station = |stations: &mut Stations, p: &Place, coordinate: usize| match p.stop_id {
            Some(_) => stations.intern(sighting(resolved, &p.name, p.tz.as_deref(), p.lat.zip(p.lon), p.station_id())),
            None => coordinate,
        };
        let mut previous_end: Option<usize> = None;
        let mut walk = TimeDelta::zero();
        for leg in &itinerary.legs {
            let Some(trip_id) = &leg.trip_id else {
                // MOTIS times are real dates, so only absurd walk sums saturate.
                walk = checked::sum(walk, leg.duration());
                continue;
            };
            let calls = leg_calls(leg, resolved, stations);
            let (Some(first), Some(final_call)) = (calls.first(), calls.last()) else { continue };
            let from = match previous_end {
                Some(end) => Some(end),
                None if walk > TimeDelta::zero() => {
                    itinerary.legs.first().map(|l| end_station(stations, &l.from, origin_idx))
                }
                None => None,
            };
            if let Some(from) = from {
                add_walk(&mut rough.footpaths, from, first.station, walk);
            }
            previous_end = Some(final_call.station);
            walk = TimeDelta::zero();

            let stops: Vec<&Call> = match leg.class() {
                Some(class) if all_stops && class != 'S' => calls.iter().collect(),
                Some(_) => vec![first, final_call],
                None => Vec::new(),
            };
            for call in stops {
                if let Some(t) = call.planned() {
                    let range = rough.corridor.entry(call.station).or_insert((t, t));
                    *range = (range.0.min(t), range.1.max(t));
                }
            }

            let entry = rough.trips.entry(trip_id.clone()).or_insert_with(|| Trip {
                label: leg.label(),
                numbers: leg.train_number().into_iter().collect(),
                class: leg.class(),
                headsign: leg.headsign.clone(),
                calls: Vec::new(),
                transitous: true,
            });
            // Legs of one trip from different itineraries meet where one ends
            // (arrival only) and another starts (departure only).
            for call in calls {
                match entry.calls.iter_mut().find(|c| c.station == call.station) {
                    Some(known) => {
                        known.arrival = known.arrival.or(call.arrival);
                        known.departure = known.departure.or(call.departure);
                        if known.platform.is_none() {
                            known.platform = call.platform;
                        }
                    }
                    None => entry.calls.push(call),
                }
            }
        }
        if let (Some(from), Some(last)) = (previous_end, itinerary.legs.last().filter(|l| !l.is_transit())) {
            let to = end_station(stations, &last.to, destination_idx);
            add_walk(&mut rough.footpaths, from, to, walk);
        }
    }
    rough
}

/// Trains with at least two calls, in time order, delays carried forward.
fn finish(trips: Vec<Trip>) -> Vec<Trip> {
    trips
        .into_iter()
        .filter(|t| t.calls.len() >= 2)
        .map(|mut t| {
            t.calls.sort_by_key(Call::planned);
            propagate_delays(&mut t.calls);
            t
        })
        .collect()
}

/// The walks from MOTIS, plus walks between nearby stations kept apart
/// (different EVA numbers), sorted by (from, to).
fn footpaths(stations: &[Station], mut walks: HashMap<(usize, usize), TimeDelta>) -> Vec<Footpath> {
    for (a, sa) in stations.iter().enumerate() {
        for (b, sb) in stations.iter().enumerate().filter(|&(b, _)| b != a) {
            let (Some(pa), Some(pb)) = (sa.position, sb.position) else { continue };
            if meters(pa, pb) <= SAME_STATION_METERS {
                walks.entry((a, b)).or_insert(NEARBY_WALK);
            }
        }
    }
    let mut footpaths: Vec<Footpath> =
        walks.into_iter().map(|((from, to), duration)| Footpath { from, to, duration }).collect();
    footpaths.sort_by_key(|f| (f.from, f.to));
    footpaths
}

/// MOTIS stops merged into a DB train often lack live data while DB's stops
/// have it; carry the last known delay forward so a delayed train doesn't
/// arrive at the next stop before it left the previous one.
///
/// Live data also contradicts itself: DB has no forecasts abroad, so IC 5762
/// leaves Padborg 34 minutes late and is "on time" in Kolding, 55 minutes
/// later; an RE7's forecast had it reach Jübek in 0 of its 6 minutes from
/// Schleswig. A train makes up at most a quarter of a stretch's planned time
/// (`MAX_CATCH_UP`; dwell times may shrink to nothing), so such live times
/// are put back.
fn propagate_delays(calls: &mut [Call]) {
    let mut delay = TimeDelta::zero();
    // The last time, and the train's last time at the stop before.
    let mut previous: Option<Time> = None;
    let mut left: Option<Time> = None;
    for call in calls {
        let from_board = call.from_board;
        let mut first = true;
        for time in [&mut call.arrival, &mut call.departure].into_iter().flatten() {
            if !(from_board || time.actual != time.planned) {
                // Only absurd feed delays reach past representable time; such a
                // stop is then later than anything, i.e. unreachable.
                time.actual = checked::later_or_max(time.planned, delay.max(TimeDelta::zero()));
            }
            if let Some(left) = left.filter(|_| first) {
                time.actual = time.actual.max(checked::later_or_max(left.actual, fastest_ride(left, *time)));
            }
            if let Some(previous) = previous.filter(|p| time.actual < p.actual) {
                time.actual = previous.actual;
            }
            delay = time.delay();
            previous = Some(*time);
            first = false;
        }
        left = previous;
    }
}

/// The shortest a ride from `left` to `time` can take: its planned time less
/// the whole minutes a train can make up on it.
fn fastest_ride(left: Time, time: Time) -> TimeDelta {
    let planned = time.planned.signed_duration_since(left.planned);
    let catch_up = planned.num_minutes().checked_div(MAX_CATCH_UP).unwrap_or(0);
    checked::difference(planned, TimeDelta::try_minutes(catch_up).unwrap_or_default())
}

fn leg_places(leg: &Leg) -> Vec<&Place> {
    std::iter::once(&leg.from).chain(&leg.intermediate_stops).chain(std::iter::once(&leg.to)).collect()
}

/// A MOTIS leg's stops as calls: no arrival where boarded, no departure where left.
fn leg_calls(leg: &Leg, resolved: &HashMap<String, db::Station>, stations: &mut Stations) -> Vec<Call> {
    let places = leg_places(leg);
    let last = places.len().saturating_sub(1);
    // Ids of rail stops only: departures at a bus stop named "Berlin Hbf" lack the trains.
    let rail = leg.class().is_some();
    places
        .iter()
        .enumerate()
        .map(|(k, p)| Call {
            station: stations.intern(sighting(
                resolved,
                &p.name,
                p.tz.as_deref(),
                p.lat.zip(p.lon),
                p.station_id().filter(|_| rail),
            )),
            arrival: if k > 0 { Time::new(p.scheduled_arrival, p.arrival) } else { None },
            departure: if k < last { Time::new(p.scheduled_departure, p.departure) } else { None },
            platform: p.track.clone(),
            cancelled: p.cancelled,
            from_board: false,
        })
        // Stops passed without times (Limburg Süd for ICE 616) can't be used.
        .filter(|c| c.arrival.is_some() || c.departure.is_some())
        .collect()
}

/// Adds the stops of `leg`, the same train from Transitous, that `trip` (known
/// from DB boards only) lacks between its first and last known call: for the
/// display, after the search. Nothing is added if the two disagree on a time
/// where both stop. Returns where each old call went, if anything was added.
pub fn add_stops(tt: &mut Timetable, trip: usize, leg: &Leg) -> Option<Vec<usize>> {
    let mut stations = Stations::from_list(std::mem::take(&mut tt.stations));
    let calls = leg_calls(leg, &HashMap::new(), &mut stations);
    tt.stations = stations.list;
    let known = &tt.trips.get(trip)?.calls;
    let close = |a: Option<Time>, b: Option<Time>| {
        a.zip(b).is_none_or(|(a, b)| a.planned.signed_duration_since(b.planned).abs() <= SAME_TIME)
    };
    let mut shared = false;
    for call in &calls {
        if let Some(k) = known.iter().find(|k| k.station == call.station) {
            if !(close(call.arrival, k.arrival) && close(call.departure, k.departure)) {
                return None;
            }
            shared = true;
        }
    }
    let times = known.iter().filter_map(Call::planned);
    let (lo, hi) = times.clone().min().zip(times.max())?;
    let added: Vec<Call> = calls
        .into_iter()
        .filter(|c| known.iter().all(|k| k.station != c.station))
        .filter(|c| c.planned().is_some_and(|t| lo < t && t < hi))
        .collect();
    if !shared || added.is_empty() {
        return None;
    }
    let trip = tt.trips.get_mut(trip)?;
    let old = std::mem::take(&mut trip.calls).into_iter().enumerate().map(|(i, c)| (Some(i), c));
    let mut all: Vec<(Option<usize>, Call)> = old.chain(added.into_iter().map(|c| (None, c))).collect();
    all.sort_by_key(|(_, c)| c.planned());
    let mut remap = vec![0; all.iter().filter(|(old, _)| old.is_some()).count()];
    for (new, (old, _)) in all.iter().enumerate() {
        if let Some(slot) = old.and_then(|old| remap.get_mut(old)) {
            *slot = new;
        }
    }
    trip.calls = all.into_iter().map(|(_, c)| c).collect();
    // The added stops' times follow DB's delays the same way as every train's.
    propagate_delays(&mut trip.calls);
    Some(remap)
}

/// Fetched DB boards per station index.
type Boards = HashMap<usize, (Vec<Visit>, Coverage)>;

/// Hour starts covering `[lo - margin, hi + margin]`, capped.
fn hours(lo: DateTime<Utc>, hi: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let Some(Ok(mut t)) = lo.checked_sub_signed(BOARD_MARGIN).map(|t| t.duration_trunc(TimeDelta::hours(1))) else {
        return Vec::new();
    };
    let end = checked::later_or_max(hi, BOARD_MARGIN);
    let mut out = Vec::new();
    while t <= end && out.len() < MAX_HOURS_PER_STATION {
        out.push(t);
        let Some(next) = t.checked_add_signed(TimeDelta::hours(1)) else { break };
        t = next;
    }
    out
}

async fn load_boards(db: &Db, wanted: Vec<(usize, String, Vec<DateTime<Utc>>)>, warnings: &mut Vec<String>) -> Boards {
    let fetches =
        wanted.into_iter().map(|(idx, eva, hours)| async move { (idx, eva.clone(), db.visits(&eva, &hours).await) });
    let mut boards = Boards::new();
    for (idx, eva, result) in join_all(fetches).await {
        match result {
            Ok(board) => {
                boards.insert(idx, board);
            }
            Err(e) => warnings.push(format!("DB board {eva}: {e:#}")),
        }
    }
    boards
}

/// Boards for corridor stations, plus stations where DB long-distance trains
/// call before reaching the next corridor station (e.g. Frankfurt Süd when
/// MOTIS routes the train via Frankfurt Hbf).
async fn fetch_boards(
    motis: &Motis,
    db: &Db,
    stations: &mut Stations,
    corridor: &HashMap<usize, (DateTime<Utc>, DateTime<Utc>)>,
    discovered: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Boards {
    let wanted: Vec<_> = corridor
        .iter()
        .filter_map(|(&idx, &(lo, hi))| Some((idx, stations.list.get(idx)?.eva.clone()?, hours(lo, hi))))
        .collect();
    let mut boards = load_boards(db, wanted, warnings).await;

    let by_db_name: HashMap<String, usize> =
        boards.keys().filter_map(|&idx| Some((stations.list.get(idx)?.board_name.clone()?, idx))).collect();
    let mut between: HashMap<String, (usize, DateTime<Utc>, DateTime<Utc>)> = HashMap::new();
    for (visits, _) in boards.values() {
        for visit in visits.iter().filter(|v| v.class == Some('F')) {
            let Some(departure) = &visit.departure else { continue };
            let Some(t_from) = departure.planned else { continue };
            let path = departure.path();
            let Some((next, next_idx)) =
                path.iter().enumerate().find_map(|(i, name)| Some((i, *by_db_name.get(name)?)))
            else {
                continue;
            };
            let t_to = boards
                .get(&next_idx)
                .and_then(|(later, _)| later.iter().find(|w| w.trip == visit.trip))
                .and_then(|w| w.arrival.as_ref()?.planned)
                // Only a range to fetch boards for; past representable time it is open-ended.
                .unwrap_or_else(|| checked::later_or_max(t_from, TimeDelta::hours(1)));
            for name in path.iter().take(next) {
                let entry = between.entry(name.clone()).or_insert((0, t_from, t_to));
                *entry = (entry.0.saturating_add(1), entry.1.min(t_from), entry.2.max(t_to));
            }
        }
    }
    let mut between: Vec<_> = between.into_iter().collect();
    between.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
    between.truncate(MAX_DISCOVERED);

    // DB station plus position and id from MOTIS, so feeds naming it differently
    // unify, and trains there can be looked up. A stop MOTIS lacks is no error;
    // failing to ask is, as the station then has no position or id.
    let lookups = between.iter().map(|(name, _)| async move {
        let station = db.resolve(name, None).await;
        let stop = motis.find_stop(name).await.map(|(stop, _)| stop.filter(|s| db::same_place(&s.name, name)));
        (station, stop)
    });
    let mut wanted = Vec::new();
    for ((name, (_, lo, hi)), (result, stop)) in between.iter().zip(join_all(lookups).await) {
        let stop = stop.unwrap_or_else(|e| {
            warnings.push(format!("MOTIS stop lookup for {name}: {e:#}"));
            None
        });
        match result {
            Ok(Some(station)) => {
                let idx = stations.intern(Sighting {
                    name: &station.name,
                    eva: Some(&station.eva),
                    tv: None,
                    board_name: Some(&station.name),
                    tz: Some(DB_TIME_ZONE),
                    position: stop.as_ref().and_then(|s| s.position),
                    motis_id: stop.as_ref().map(|s| s.id.as_str()),
                });
                if !boards.contains_key(&idx) {
                    discovered.push(station.name.clone());
                    wanted.push((idx, station.eva, hours(*lo, *hi)));
                }
            }
            Ok(None) => warnings.push(format!("DB has no station named {name}")),
            Err(e) => warnings.push(format!("DB station lookup for {name}: {e:#}")),
        }
    }
    boards.extend(load_boards(db, wanted, warnings).await);
    boards
}

/// A train known in full from a board feed, with its route's station names.
type Fed = (Trip, HashSet<String>);

/// What Trafikverket contributes: its trains, and per boarded station which
/// classes its board lists by the hour.
#[derive(Default)]
struct Swedish {
    trips: Vec<Fed>,
    coverage: Vec<(usize, Coverage)>,
}

/// The Trafikverket station at `position` (or named `name`), Swedish only:
/// Trafikverket's foreign stations list only Swedish trains.
fn tv_station<'a>(
    list: &'a [trafikverket::Station],
    position: Option<(f64, f64)>,
    name: &str,
) -> Option<&'a trafikverket::Station> {
    let swedish = list.iter().filter(|s| s.country == "SE");
    let near = position.and_then(|here| {
        swedish
            .clone()
            .filter_map(|s| Some((s, meters(s.position?, here))))
            .filter(|&(_, m)| m <= TV_SAME_STATION_METERS)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(s, _)| s)
    });
    near.or_else(|| swedish.clone().find(|s| db::fold(&s.name) == db::fold(name)))
}

/// Board hours from `lo` to `hi` widened by `BOARD_MARGIN`, at most `MAX_HOURS_PER_STATION`.
fn board_span(lo: DateTime<Utc>, hi: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let from = checked::earlier_or_min(lo, BOARD_MARGIN);
    let longest = TimeDelta::hours(i64::try_from(MAX_HOURS_PER_STATION).unwrap_or(i64::MAX));
    let until = checked::later_or_max(hi, BOARD_MARGIN).min(checked::later_or_max(from, longest));
    (from, until)
}

/// Trains at the boards of `wanted` (signature → span): per signature what it
/// lists, and the runs not in `known` yet (in `known` from then on).
async fn tv_boards(
    tv: &Trafikverket,
    wanted: &HashMap<String, (DateTime<Utc>, DateTime<Utc>)>,
    known: &mut HashSet<trafikverket::RunKey>,
    keep: impl Fn(&trafikverket::Seen) -> bool,
    warnings: &mut Vec<String>,
) -> (HashMap<String, Vec<trafikverket::Seen>>, Vec<trafikverket::RunKey>) {
    let fetches = wanted.iter().map(|(sig, &(lo, hi))| async move {
        let (from, until) = board_span(lo, hi);
        (sig.clone(), tv.board(sig, from, until).await)
    });
    let mut listed = HashMap::new();
    let mut new = Vec::new();
    for (sig, result) in join_all(fetches).await {
        match result {
            Ok(seen) => {
                for s in seen.iter().filter(|s| keep(s)) {
                    if known.insert(s.run.clone()) {
                        new.push(s.run.clone());
                    }
                }
                listed.insert(sig, seen);
            }
            Err(e) => warnings.push(format!("Trafikverket board {sig}: {e:#}")),
        }
    }
    new.sort();
    (listed, new)
}

/// Trains in Sweden from Trafikverket: the whole runs of every train at the
/// corridor's Swedish stations; then the boards of stations those runs share
/// (junctions such as Alvesta or Emmaboda, by how many long-distance and
/// regional runs call there, near the line between the ends), and the runs of
/// their long-distance and regional trains. A planner's answers name neither.
async fn swedish(
    tv: &Trafikverket,
    stations: &mut Stations,
    corridor: &HashMap<usize, (DateTime<Utc>, DateTime<Utc>)>,
    ends: (usize, usize),
    discovered: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Swedish {
    if corridor.is_empty() {
        return Swedish::default();
    }
    // Which corridor stations are Swedish only the station list tells (cached
    // for a week): Transitous gives Göteborg the time zone Europe/Berlin.
    let mut corridor: Vec<_> = corridor.iter().collect();
    corridor.sort_by_key(|(idx, _)| **idx);
    let list = match tv.stations().await {
        Ok(list) => list,
        Err(e) => {
            warnings.push(format!("Trafikverket stations: {e:#}"));
            return Swedish::default();
        }
    };
    let by_sig: HashMap<&str, &trafikverket::Station> = list.iter().map(|s| (s.signature.as_str(), s)).collect();
    let mut first: HashMap<String, (DateTime<Utc>, DateTime<Utc>)> = HashMap::new();
    let mut boarded: HashMap<String, usize> = HashMap::new();
    for (&idx, &(lo, hi)) in corridor {
        let Some(st) = stations.list.get(idx) else { continue };
        let Some(found) = tv_station(&list, st.position, &st.name) else { continue };
        stations.claim(idx, found);
        boarded.insert(found.signature.clone(), idx);
        let span = first.entry(found.signature.clone()).or_insert((lo, hi));
        *span = (span.0.min(lo), span.1.max(hi));
    }
    if first.is_empty() {
        return Swedish::default();
    }
    let mut known = HashSet::new();
    let (mut listed, keys) = tv_boards(tv, &first, &mut known, |_| true, warnings).await;
    let mut runs = match tv.runs(&keys).await {
        Ok(runs) => runs,
        Err(e) => {
            warnings.push(format!("Trafikverket runs: {e:#}"));
            Vec::new()
        }
    };

    let line =
        stations.list.get(ends.0).and_then(|s| s.position).zip(stations.list.get(ends.1).and_then(|s| s.position));
    let second = change_points(&runs, &by_sig, line, &first);
    let (more, keys) = tv_boards(tv, &second, &mut known, |s| s.class != 'S', warnings).await;
    match tv.runs(&keys).await {
        Ok(found) => runs.extend(found),
        Err(e) => warnings.push(format!("Trafikverket runs: {e:#}")),
    }
    let mut found: Vec<String> = more.keys().filter_map(|sig| Some(by_sig.get(sig.as_str())?.name.clone())).collect();
    found.sort();
    discovered.extend(found);
    listed.extend(more);

    let trips = runs.iter().map(|run| fed_trip(run, &by_sig, stations)).collect();
    let mut coverage = Vec::new();
    for (sig, seen) in &listed {
        let idx = boarded.get(sig).copied().or_else(|| stations.by_key.get(&format!("tv:{sig}")).copied());
        if let Some(idx) = idx {
            // Trafikverket announces every train: an hour its board has any
            // covers all classes, whatever class another feed gives a train.
            let entries: Vec<(DateTime<Utc>, char)> =
                seen.iter().flat_map(|s| ['F', 'N', 'S'].map(|class| (s.planned, class))).collect();
            coverage.push((idx, Coverage::of(&entries)));
        }
    }
    Swedish { trips, coverage }
}

/// What long-distance and regional runs show of a station.
struct Sighted<'a> {
    /// The stations runs come from or go to.
    neighbours: HashSet<&'a str>,
    /// Some run starts or ends here.
    terminus: bool,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
}

/// Change points on long-distance and regional `runs`, off the boards so far
/// (`boarded`) and near the `line` between the ends:
/// - junctions, where runs come from or go to in at least
///   `JUNCTION_DIRECTIONS` directions. A station on one line has two
///   however many trains stop (an express's neighbours lie further along the
///   same ones); Alvesta has four;
/// - termini, where runs start or end: other lines' trains go on from there.
///   Karlskrona, where the Öresundståg from Karlshamn ends, has the Krösatåg
///   from Emmaboda, which no other board lists.
///
/// Junctions with the most directions first, at most `MAX_TV_DISCOVERED`, and
/// termini nearest the destination first, at most `MAX_TV_TERMINI` (near the
/// origin, Transitous's answers and the boards there cover what goes on).
/// Each with the span to fetch its board for: from the first run there to
/// `TV_ONWARD` after the last.
fn change_points(
    runs: &[trafikverket::Run],
    by_sig: &HashMap<&str, &trafikverket::Station>,
    line: Option<((f64, f64), (f64, f64))>,
    boarded: &HashMap<String, (DateTime<Utc>, DateTime<Utc>)>,
) -> HashMap<String, (DateTime<Utc>, DateTime<Utc>)> {
    let mut seen: HashMap<&str, Sighted<'_>> = HashMap::new();
    for run in runs.iter().filter(|r| r.class != 'S') {
        let last = run.stops.len().saturating_sub(1);
        for (k, stop) in run.stops.iter().enumerate() {
            let Some(t) = stop.arrival.or(stop.departure).map(|m| m.planned) else { continue };
            let Some(station) = by_sig.get(stop.signature.as_str()) else { continue };
            let near_line = match (line, station.position) {
                (Some((o, d)), Some(p)) => meters(o, p) + meters(p, d) <= MAX_DETOUR * meters(o, d),
                _ => true,
            };
            if station.country != "SE" || !near_line || boarded.contains_key(&stop.signature) {
                continue;
            }
            let entry = seen.entry(stop.signature.as_str()).or_insert_with(|| Sighted {
                neighbours: HashSet::new(),
                terminus: false,
                first: t,
                last: t,
            });
            let neighbours = [k.checked_sub(1), k.checked_add(1)].into_iter().flatten();
            entry.neighbours.extend(neighbours.filter_map(|n| run.stops.get(n)).map(|s| s.signature.as_str()));
            entry.terminus |= k == 0 || k == last;
            (entry.first, entry.last) = (entry.first.min(t), entry.last.max(t));
        }
    }
    let mut ranked: Vec<_> = seen
        .into_iter()
        .filter_map(|(sig, seen)| {
            let here = by_sig.get(sig)?.position?;
            let mut bearings: Vec<f64> =
                seen.neighbours.iter().filter_map(|n| Some(bearing(here, by_sig.get(n)?.position?))).collect();
            bearings.sort_by(f64::total_cmp);
            let mut directions: Vec<f64> = Vec::new();
            for b in bearings {
                if directions.iter().all(|&d| angle_between(b, d) > SAME_DIRECTION_DEGREES) {
                    directions.push(b);
                }
            }
            let to_go = line.map_or(0.0, |(_, d)| meters(here, d));
            Some((sig, directions.len(), seen.terminus, to_go, seen.first, seen.last))
        })
        .collect();
    let span = |(sig, _, _, _, lo, hi): (&str, usize, bool, f64, DateTime<Utc>, DateTime<Utc>)| {
        (sig.to_owned(), (lo, checked::later_or_max(hi, TV_ONWARD)))
    };
    let mut junctions: Vec<_> = ranked.iter().copied().filter(|s| s.1 >= JUNCTION_DIRECTIONS).collect();
    junctions.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    junctions.truncate(MAX_TV_DISCOVERED);
    ranked.retain(|s| s.1 < JUNCTION_DIRECTIONS && s.2);
    ranked.sort_by(|a, b| a.3.total_cmp(&b.3).then(a.0.cmp(b.0)));
    ranked.truncate(MAX_TV_TERMINI);
    junctions.into_iter().chain(ranked).map(span).collect()
}

/// Compass bearing from `a` to `b` in degrees, 0 north, 90 east.
pub fn bearing(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, la2, dlo) = (a.0.to_radians(), b.0.to_radians(), (b.1 - a.1).to_radians());
    let y = dlo.sin() * la2.cos();
    let x = la1.cos().mul_add(la2.sin(), -(la1.sin() * la2.cos() * dlo.cos()));
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// The smaller angle between two bearings, 0–180 degrees.
fn angle_between(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// A Trafikverket run as a train, its stops interned; stops in countries
/// other feeds' boards cover (Germany) are left out.
fn fed_trip(run: &trafikverket::Run, by_sig: &HashMap<&str, &trafikverket::Station>, stations: &mut Stations) -> Fed {
    let time =
        |m: Option<trafikverket::Moment>| m.map(|m| Time { planned: m.planned, actual: m.live.unwrap_or(m.planned) });
    let mut calls = Vec::new();
    let mut route = HashSet::new();
    for stop in &run.stops {
        let Some(station) = by_sig.get(stop.signature.as_str()) else { continue };
        if !TV_COUNTRIES.contains(&station.country.as_str()) {
            continue;
        }
        let idx = stations.intern(Sighting {
            name: &station.name,
            tv: Some(&station.signature),
            board_name: Some(&station.name),
            position: station.position,
            ..Sighting::default()
        });
        route.insert(station.name.clone());
        calls.push(Call {
            station: idx,
            arrival: time(stop.arrival),
            departure: time(stop.departure),
            platform: stop.track.clone(),
            cancelled: [stop.arrival, stop.departure].into_iter().flatten().any(|m| m.cancelled),
            from_board: true,
        });
    }
    let headsign = run.stops.last().and_then(|s| by_sig.get(s.signature.as_str())).map(|s| s.name.clone());
    let trip = Trip {
        label: run.label(),
        numbers: run.number().into_iter().collect(),
        class: Some(run.class),
        headsign,
        calls,
        transitous: false,
    };
    (trip, route)
}

/// Whole seconds of a float, rounded and clamped to what a timetable needs.
#[expect(clippy::cast_possible_truncation, reason = "rounded and clamped to a range i64 holds")]
pub const fn whole_seconds(s: f64) -> i64 {
    s.round().clamp(-1e12, 1e12) as i64
}

/// A DB train's calls as (station, planned arrival, planned departure).
type Signature = Vec<(usize, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>;
/// A train seen at a station: (trip, planned arrival, planned departure).
type Seen = (usize, Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// DB trains from the boards and `fed` trains (Trafikverket's), MOTIS trains
/// merged into them by number. The boards win where they have data: a MOTIS
/// stop is dropped if the board train's route omits the station, or the
/// station's board lists other trains of that class for the hour. Without
/// boards, copies of one train from several feeds become one, EFA's times winning.
#[expect(clippy::too_many_lines, reason = "the merge rules read best in one place, in order")]
fn merge(stations: &[Station], boards: &Boards, raw: HashMap<String, Trip>, fed: Vec<Fed>) -> Vec<Trip> {
    let mut runs: HashMap<&str, Vec<(usize, &Visit)>> = HashMap::new();
    for (&idx, (visits, _)) in boards {
        for v in visits {
            runs.entry(v.trip.as_str()).or_default().push((idx, v));
        }
    }
    // Per DB trip: every station name its route mentions.
    let mut routes: Vec<HashSet<String>> = Vec::new();
    let mut trips: Vec<Trip> = Vec::new();
    let mut runs: Vec<_> = runs.into_iter().collect();
    runs.sort_by_key(|(id, _)| *id);
    for (_, mut calls) in runs {
        calls.sort_by_key(|(_, v)| v.seq);
        let mut route = HashSet::new();
        for &(idx, v) in &calls {
            route.extend(stations.get(idx).and_then(|s| s.board_name.clone()));
            for e in [&v.arrival, &v.departure].into_iter().flatten() {
                route.extend(e.path().iter().cloned());
            }
        }
        let mut labels: HashMap<String, usize> = HashMap::new();
        for (_, v) in calls.iter().filter(|(_, v)| v.category.is_some()) {
            let count = labels.entry(v.label()).or_default();
            *count = count.saturating_add(1);
        }
        let label = labels
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map_or_else(|| calls.first().map(|(_, v)| v.label()).unwrap_or_default(), |(l, _)| l);
        let mut numbers: Vec<u32> = calls.iter().filter_map(|(_, v)| v.number).collect();
        numbers.sort_unstable();
        numbers.dedup();
        trips.push(Trip {
            label,
            numbers,
            class: calls.iter().find_map(|(_, v)| v.class),
            headsign: calls.iter().rev().find_map(|(_, v)| v.departure.as_ref()?.path().last().cloned()),
            calls: calls
                .iter()
                .map(|&(station, v)| Call {
                    station,
                    arrival: v.arrival.as_ref().and_then(Time::from_event),
                    departure: v.departure.as_ref().and_then(Time::from_event),
                    platform: v
                        .departure
                        .as_ref()
                        .and_then(Event::platform)
                        .or_else(|| v.arrival.as_ref().and_then(Event::platform))
                        .map(str::to_owned),
                    cancelled: [&v.arrival, &v.departure].into_iter().flatten().any(|e| e.cancelled),
                    from_board: true,
                })
                .collect(),
            transitous: false,
        });
        routes.push(route);
    }
    for (trip, route) in fed {
        trips.push(trip);
        routes.push(route);
    }
    // Coupled trains (e.g. ICE 774 + ICE 2808) appear as separate runs with the
    // same timetable; while they run together they are one train for us.
    let mut seen: HashMap<Signature, usize> = HashMap::new();
    let mut kept: Vec<Trip> = Vec::new();
    let mut kept_routes: Vec<HashSet<String>> = Vec::new();
    for (trip, route) in trips.into_iter().zip(routes) {
        let signature: Signature = trip
            .calls
            .iter()
            .map(|c| (c.station, c.arrival.map(|t| t.planned), c.departure.map(|t| t.planned)))
            .collect();
        if let Some((other, other_route)) =
            seen.get(&signature).and_then(|&i| kept.get_mut(i).zip(kept_routes.get_mut(i)))
        {
            other.label = format!("{} + {}", other.label, trip.label);
            other.numbers.extend(trip.numbers);
            other_route.extend(route);
        } else {
            seen.insert(signature, kept.len());
            kept.push(trip);
            kept_routes.push(route);
        }
    }
    let (mut trips, mut routes) = (kept, kept_routes);

    let mut by_number: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, t) in trips.iter().enumerate() {
        for &n in &t.numbers {
            by_number.entry(n).or_default().push(i);
        }
    }
    let covered = |call: &Call, class: Option<char>| {
        let (Some(class), Some(t)) = (class, call.planned()) else { return false };
        boards.get(&call.station).is_some_and(|(_, coverage)| coverage.covers(t, class))
    };
    // Feeds disagree on train numbers ("ICE 76" vs " (2558)"); the same
    // timetable at two stations identifies the same train too.
    let mut at_station: HashMap<usize, Vec<Seen>> = HashMap::new();
    let index = |at_station: &mut HashMap<usize, Vec<Seen>>, i: usize, calls: &[Call]| {
        for c in calls {
            let (arr, dep) = (c.arrival.map(|t| t.planned), c.departure.map(|t| t.planned));
            at_station.entry(c.station).or_default().push((i, arr, dep));
        }
    };
    let close = |a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>| matches!((a, b), (Some(a), Some(b)) if a.signed_duration_since(b).abs() <= SAME_TIME);
    for (i, t) in trips.iter().enumerate() {
        index(&mut at_station, i, &t.calls);
    }

    // Trains DB doesn't know (past days, abroad) can come from several feeds
    // with differing times (ICE 273: Transitous 22 minutes off at Mannheim).
    // EFA's go first, so that their times win and the others only add stops.
    let mut raw: Vec<(String, Trip)> = raw.into_iter().collect();
    raw.sort_by(|a, b| {
        let not_efa = |id: &str| !id.starts_with(crate::efa::TRIP_PREFIX);
        not_efa(&a.0).cmp(&not_efa(&b.0)).then_with(|| a.0.cmp(&b.0))
    });
    // Trains from those feeds by number: the same number at the same time at
    // one station is one train, whatever class the feeds gave it.
    let mut feed_by_number: HashMap<u32, Vec<usize>> = HashMap::new();
    for (_, motis) in raw {
        let by_number = motis.numbers.iter().find_map(|n| {
            by_number.get(n)?.iter().copied().find(|&i| {
                motis.calls.iter().any(|c| {
                    trips.get(i).and_then(|t| t.call_at(c.station)).is_some_and(|d| match (d.planned(), c.planned()) {
                        (Some(a), Some(b)) => a.signed_duration_since(b).abs() <= MERGE_TOLERANCE,
                        _ => false,
                    })
                })
            })
        });
        let same_run = || {
            motis.numbers.iter().find_map(|n| {
                feed_by_number.get(n)?.iter().copied().find(|&i| {
                    motis.calls.iter().any(|c| {
                        trips.get(i).and_then(|t| t.call_at(c.station)).is_some_and(|d| {
                            close(c.arrival.map(|t| t.planned), d.arrival.map(|t| t.planned))
                                || close(c.departure.map(|t| t.planned), d.departure.map(|t| t.planned))
                        })
                    })
                })
            })
        };
        let twin = by_number.or_else(same_run).or_else(|| {
            let mut shared: HashMap<usize, usize> = HashMap::new();
            for c in &motis.calls {
                let (arr, dep) = (c.arrival.map(|t| t.planned), c.departure.map(|t| t.planned));
                for &(i, other_arr, other_dep) in at_station.get(&c.station).into_iter().flatten() {
                    // An S-Bahn keeps an ICE's times between Hamburg Hbf and
                    // Altona; different product classes are different trains.
                    let same_class =
                        !matches!((motis.class, trips.get(i).and_then(|t| t.class)), (Some(a), Some(b)) if a != b);
                    if same_class && (close(arr, other_arr) || close(dep, other_dep)) {
                        let count = shared.entry(i).or_default();
                        *count = count.saturating_add(1);
                    }
                }
            }
            shared.into_iter().filter(|&(_, n)| n >= 2).max_by_key(|&(i, n)| (n, Reverse(i))).map(|(i, _)| i)
        });
        let target = twin.and_then(|i| Some((i, trips.get_mut(i)?, routes.get(i)?)));
        if let Some((i, trip, route)) = target {
            trip.transitous = true;
            // DB's route lists every stop; a stop unknown to DB between its
            // first and last call is the same station under another name
            // ("Frankfurt (Main) Flughafen Fernbahnhof") or not a stop at all.
            // So is one at the very time of the first or last call: a train
            // can't be at two stations at once.
            let db_times = trip.calls.iter().filter(|c| c.from_board).filter_map(Call::planned);
            let span = db_times.clone().min().zip(db_times.max());
            // Into another feed's copy, a stop is only taken where the copies
            // agree on the stops around it: Transitous' ICE 273 is 10 minutes
            // off at Karlsruhe, so its Mannheim time would be too.
            let from_feed = !trip.calls.iter().any(|c| c.from_board);
            let anchors: Vec<(DateTime<Utc>, bool)> = motis
                .calls
                .iter()
                .filter_map(|c| {
                    let base = trip.call_at(c.station)?;
                    let agree = close(c.arrival.map(|t| t.planned), base.arrival.map(|t| t.planned))
                        || close(c.departure.map(|t| t.planned), base.departure.map(|t| t.planned));
                    Some((c.planned()?, agree))
                })
                .collect();
            let anchored = |t: Option<DateTime<Utc>>| {
                t.is_some_and(|t| {
                    let before = anchors.iter().filter(|a| a.0 < t).max_by_key(|a| a.0);
                    let after = anchors.iter().filter(|a| a.0 > t).min_by_key(|a| a.0);
                    (before.is_some() || after.is_some()) && before.is_none_or(|a| a.1) && after.is_none_or(|a| a.1)
                })
            };
            let mut added = Vec::new();
            for call in motis.calls {
                let name = stations.get(call.station).and_then(|s| s.board_name.as_ref());
                let off_route = name.is_some_and(|n| !route.is_empty() && !route.contains(n));
                let unknown_inside = name.is_none()
                    && !route.is_empty()
                    && span.zip(call.planned()).is_some_and(|((lo, hi), t)| lo <= t && t <= hi);
                let unanchored = from_feed && !anchored(call.planned());
                if trip.call_at(call.station).is_none()
                    && !off_route
                    && !unknown_inside
                    && !unanchored
                    && !covered(&call, motis.class)
                {
                    added.push(call.clone());
                    trip.calls.push(call);
                }
            }
            index(&mut at_station, i, &added);
        } else {
            let class = motis.class;
            let calls: Vec<Call> = motis.calls.into_iter().filter(|c| !covered(c, class)).collect();
            // Only trains: buses and trams carry line numbers, shared by many runs.
            if class.is_some() {
                for &n in &motis.numbers {
                    feed_by_number.entry(n).or_default().push(trips.len());
                }
            }
            index(&mut at_station, trips.len(), &calls);
            trips.push(Trip { calls, ..motis });
            routes.push(HashSet::new());
        }
    }
    trips
}

#[cfg(test)]
#[path = "timetable_tests.rs"]
mod tests;
