//! A small timetable for the corridor of one search: trains from MOTIS rough
//! routes, overridden and extended by DB station boards where available.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use futures::future::join_all;

use crate::db::{self, Coverage, Db, Event, Visit};
use crate::motis::{Itinerary, Leg, Motis, Place, Stop};

/// Same train number within this time difference at a shared station = same train.
const MERGE_TOLERANCE: TimeDelta = TimeDelta::minutes(60);
/// Without a matching number, two shared stations within this difference = same train.
const SAME_TIME: TimeDelta = TimeDelta::minutes(2);
/// Boards are fetched this far around the times MOTIS has for a station.
const BOARD_MARGIN: TimeDelta = TimeDelta::minutes(30);
const MAX_HOURS_PER_STATION: usize = 10;
/// Cap on stations added because DB trains call there between corridor stations.
const MAX_DISCOVERED: usize = 20;
/// Stops of different feeds closer than this are one station.
const SAME_STATION_METERS: f64 = 300.0;
/// Walking speed and minimum walk between nearby stations without MOTIS footpath.
const WALK_METERS_PER_SECOND: f64 = 1.0;
const MIN_WALK: TimeDelta = TimeDelta::minutes(5);
/// DB boards cover German stations; elsewhere they are empty.
const DB_TIME_ZONE: &str = "Europe/Berlin";

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
        self.actual - self.planned
    }
}

#[derive(Debug, Clone)]
pub struct Call {
    pub station: usize,
    pub arrival: Option<Time>,
    pub departure: Option<Time>,
    pub platform: Option<String>,
    pub cancelled: bool,
    pub from_db: bool,
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
    /// Name in DB's data, as used in train paths.
    pub db_name: Option<String>,
    pub tz: Option<String>,
    pub position: Option<(f64, f64)>,
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

pub struct Build {
    pub timetable: Timetable,
    /// Stations added from DB train paths.
    pub discovered: Vec<String>,
    pub warnings: Vec<String>,
}

/// What is known about a station when it is first seen.
#[derive(Default)]
struct Sighting<'a> {
    name: &'a str,
    eva: Option<&'a str>,
    db_name: Option<&'a str>,
    tz: Option<&'a str>,
    position: Option<(f64, f64)>,
}

#[derive(Default)]
struct Stations {
    list: Vec<Station>,
    by_key: HashMap<String, usize>,
}

impl Stations {
    /// One index per physical station, unified by EVA number, folded name or
    /// proximity. Nearby stations with different EVA numbers stay separate
    /// (S-Bahn vs. long-distance station) and get a footpath instead.
    fn intern(&mut self, s: Sighting<'_>) -> usize {
        let name_key = format!("name:{}", db::fold(s.name));
        let eva_key = s.eva.map(|e| format!("eva:{e}"));
        let found = eva_key
            .as_ref()
            .and_then(|k| self.by_key.get(k))
            .or_else(|| self.by_key.get(&name_key))
            .copied()
            .or_else(|| {
                let here = s.position?;
                self.list.iter().position(|st| {
                    let compatible = st.eva.is_none() || s.eva.is_none();
                    compatible && st.position.is_some_and(|p| meters(p, here) <= SAME_STATION_METERS)
                })
            });
        let idx = found.unwrap_or_else(|| {
            self.list.push(Station {
                name: s.name.to_owned(),
                eva: None,
                db_name: None,
                tz: None,
                position: None,
            });
            self.list.len() - 1
        });
        let station = &mut self.list[idx];
        if station.eva.is_none() {
            station.eva = s.eva.map(str::to_owned);
        }
        if station.db_name.is_none() {
            station.db_name = s.db_name.map(str::to_owned);
        }
        if station.tz.is_none() {
            station.tz = s.tz.map(str::to_owned);
        }
        if station.position.is_none() {
            station.position = s.position;
        }
        self.by_key.insert(name_key, idx);
        if let Some(k) = eva_key {
            self.by_key.insert(k, idx);
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
) -> Sighting<'a> {
    let db = resolved.get(name);
    Sighting {
        name,
        eva: db.map(|s| s.eva.as_str()),
        db_name: db.map(|s| s.name.as_str()),
        tz,
        position,
    }
}

/// Great-circle distance between (lat, lon) pairs.
pub fn meters(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (a.0.to_radians(), a.1.to_radians(), b.0.to_radians(), b.1.to_radians());
    let h = ((la2 - la1) / 2.0).sin().powi(2) + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * h.sqrt().asin()
}

/// `all_stops`: fetch DB boards for every stop of the rough routes' trains,
/// not only where they start or end. More change points (e.g. RE7 to
/// Schleswig to catch the RJ there), roughly three times the DB requests.
pub async fn build(
    motis: &Motis,
    db: Option<&Db>,
    itineraries: &[Itinerary],
    origin: &Stop,
    destination: &Stop,
    all_stops: bool,
) -> Build {
    let mut warnings = Vec::new();

    // DB station for every German rail station MOTIS mentions.
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
    let mut resolved: HashMap<String, db::Station> = HashMap::new();
    if let Some(db) = db {
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
    let sighting = |name, tz, position| sighting(&resolved, name, tz, position);

    let mut stations = Stations::default();
    let origin_idx = stations.intern(sighting(&origin.name, None, origin.position));
    let destination_idx = stations.intern(sighting(&destination.name, None, destination.position));

    // Trains and walks from MOTIS.
    let mut raw: HashMap<String, Trip> = HashMap::new();
    // Stations of rail legs, with the time span MOTIS has there; their DB boards are fetched.
    let mut corridor: HashMap<usize, (DateTime<Utc>, DateTime<Utc>)> = HashMap::new();
    let mut footpaths: HashMap<(usize, usize), TimeDelta> = HashMap::new();
    for itinerary in itineraries {
        // Walks at the ends lead from / to the queried place: coordinates
        // (no stop id) stand for our origin or destination, stops for a hub.
        let end_station = |stations: &mut Stations, p: &Place, coordinate: usize| match p.stop_id {
            Some(_) => stations.intern(self::sighting(&resolved, &p.name, p.tz.as_deref(), p.lat.zip(p.lon))),
            None => coordinate,
        };
        let mut previous_end: Option<usize> = None;
        let mut walk = TimeDelta::zero();
        for leg in &itinerary.legs {
            let Some(trip_id) = &leg.trip_id else {
                walk += leg.duration();
                continue;
            };
            let places = leg_places(leg);
            let last = places.len() - 1;
            let calls: Vec<Call> = places
                .iter()
                .enumerate()
                .map(|(k, p)| Call {
                    station: stations.intern(sighting(&p.name, p.tz.as_deref(), p.lat.zip(p.lon))),
                    arrival: if k > 0 { Time::new(p.scheduled_arrival, p.arrival) } else { None },
                    departure: if k < last { Time::new(p.scheduled_departure, p.departure) } else { None },
                    platform: p.track.clone(),
                    cancelled: p.cancelled,
                    from_db: false,
                })
                .collect();
            let from = match previous_end {
                Some(end) => Some(end),
                None if walk > TimeDelta::zero() => Some(end_station(&mut stations, &itinerary.legs[0].from, origin_idx)),
                None => None,
            };
            if let Some(from) = from.filter(|&from| from != calls[0].station) {
                let entry = footpaths.entry((from, calls[0].station)).or_insert(walk);
                *entry = (*entry).min(walk);
            }
            previous_end = Some(calls[last].station);
            walk = TimeDelta::zero();

            let stops: Vec<&Call> = match leg.class() {
                Some(class) if all_stops && class != 'S' => calls.iter().collect(),
                Some(_) => vec![&calls[0], &calls[last]],
                None => Vec::new(),
            };
            for call in stops {
                if let Some(t) = call.planned() {
                    let range = corridor.entry(call.station).or_insert((t, t));
                    *range = (range.0.min(t), range.1.max(t));
                }
            }

            let entry = raw.entry(trip_id.clone()).or_insert_with(|| Trip {
                label: leg.label(),
                numbers: leg.train_number().into_iter().collect(),
                class: leg.class(),
                headsign: leg.headsign.clone(),
                calls: Vec::new(),
            });
            for call in calls {
                if entry.call_at(call.station).is_none() {
                    entry.calls.push(call);
                }
            }
        }
        if let (Some(from), Some(last)) = (previous_end, itinerary.legs.last().filter(|l| !l.is_transit())) {
            let to = end_station(&mut stations, &last.to, destination_idx);
            if from != to {
                let entry = footpaths.entry((from, to)).or_insert(walk);
                *entry = (*entry).min(walk);
            }
        }
    }

    let mut discovered = Vec::new();
    let trips = match db {
        Some(db) => {
            let boards = fetch_boards(motis, db, &mut stations, &corridor, &mut discovered, &mut warnings).await;
            merge(&stations.list, &boards, raw)
        }
        None => raw.into_values().collect(),
    };
    let trips = trips
        .into_iter()
        .filter(|t| t.calls.len() >= 2)
        .map(|mut t| {
            t.calls.sort_by_key(Call::planned);
            propagate_delays(&mut t.calls);
            t
        })
        .collect();

    // Nearby stations kept apart (different EVA numbers) are a short walk apart.
    for (a, sa) in stations.list.iter().enumerate() {
        for (b, sb) in stations.list.iter().enumerate().filter(|&(b, _)| b != a) {
            let (Some(pa), Some(pb)) = (sa.position, sb.position) else { continue };
            let distance = meters(pa, pb);
            if distance <= SAME_STATION_METERS {
                let walk = TimeDelta::seconds((distance / WALK_METERS_PER_SECOND) as i64).max(MIN_WALK);
                footpaths.entry((a, b)).or_insert(walk);
            }
        }
    }
    let mut footpaths: Vec<Footpath> = footpaths
        .into_iter()
        .map(|((from, to), duration)| Footpath { from, to, duration })
        .collect();
    footpaths.sort_by_key(|f| (f.from, f.to));

    Build {
        timetable: Timetable {
            stations: stations.list,
            trips,
            footpaths,
            origin: origin_idx,
            destination: destination_idx,
        },
        discovered,
        warnings,
    }
}

/// MOTIS stops merged into a DB train often lack live data while DB's stops
/// have it; carry the last known delay forward so a delayed train doesn't
/// arrive at the next stop before it left the previous one.
fn propagate_delays(calls: &mut [Call]) {
    let mut delay = TimeDelta::zero();
    let mut previous: Option<DateTime<Utc>> = None;
    for call in calls {
        let from_db = call.from_db;
        for time in [&mut call.arrival, &mut call.departure].into_iter().flatten() {
            if from_db || time.actual != time.planned {
                delay = time.delay();
            } else {
                time.actual = time.planned + delay.max(TimeDelta::zero());
            }
            if let Some(previous) = previous.filter(|&p| time.actual < p) {
                time.actual = previous;
            }
            previous = Some(time.actual);
        }
    }
}

fn leg_places(leg: &Leg) -> Vec<&Place> {
    std::iter::once(&leg.from)
        .chain(&leg.intermediate_stops)
        .chain(std::iter::once(&leg.to))
        .collect()
}

/// Fetched DB boards per station index.
type Boards = HashMap<usize, (Vec<Visit>, Coverage)>;

/// Hour starts covering `[lo - margin, hi + margin]`, capped.
fn hours(lo: DateTime<Utc>, hi: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let Ok(mut t) = (lo - BOARD_MARGIN).duration_trunc(TimeDelta::hours(1)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while t <= hi + BOARD_MARGIN && out.len() < MAX_HOURS_PER_STATION {
        out.push(t);
        t += TimeDelta::hours(1);
    }
    out
}

async fn load_boards(db: &Db, wanted: Vec<(usize, String, Vec<DateTime<Utc>>)>, warnings: &mut Vec<String>) -> Boards {
    let fetches = wanted
        .into_iter()
        .map(|(idx, eva, hours)| async move { (idx, eva.clone(), db.visits(&eva, &hours).await) });
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
        .filter_map(|(&idx, &(lo, hi))| Some((idx, stations.list[idx].eva.clone()?, hours(lo, hi))))
        .collect();
    let mut boards = load_boards(db, wanted, warnings).await;

    let by_db_name: HashMap<String, usize> = boards
        .keys()
        .filter_map(|&idx| Some((stations.list[idx].db_name.clone()?, idx)))
        .collect();
    let mut between: HashMap<String, (usize, DateTime<Utc>, DateTime<Utc>)> = HashMap::new();
    for (visits, _) in boards.values() {
        for visit in visits.iter().filter(|v| v.class == Some('F')) {
            let Some(departure) = &visit.departure else { continue };
            let Some(t_from) = departure.planned else { continue };
            let path = departure.path();
            let Some(next) = path.iter().position(|name| by_db_name.contains_key(name)) else {
                continue;
            };
            let t_to = boards
                .get(&by_db_name[&path[next]])
                .and_then(|(later, _)| later.iter().find(|w| w.trip == visit.trip))
                .and_then(|w| w.arrival.as_ref()?.planned)
                .unwrap_or(t_from + TimeDelta::hours(1));
            for name in &path[..next] {
                let entry = between.entry(name.clone()).or_insert((0, t_from, t_to));
                *entry = (entry.0 + 1, entry.1.min(t_from), entry.2.max(t_to));
            }
        }
    }
    let mut between: Vec<_> = between.into_iter().collect();
    between.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
    between.truncate(MAX_DISCOVERED);

    // DB station plus a position from MOTIS, so feeds naming it differently unify.
    let lookups = between.iter().map(|(name, _)| async move {
        let station = db.resolve(name, None).await;
        let position = motis
            .stop(name)
            .await
            .ok()
            .filter(|s| db::same_place(&s.name, name))
            .and_then(|s| s.position);
        (station, position)
    });
    let mut wanted = Vec::new();
    for ((name, (_, lo, hi)), (result, position)) in between.iter().zip(join_all(lookups).await) {
        match result {
            Ok(Some(station)) => {
                let idx = stations.intern(Sighting {
                    name: &station.name,
                    eva: Some(&station.eva),
                    db_name: Some(&station.name),
                    tz: Some(DB_TIME_ZONE),
                    position,
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

/// DB trains from the boards, MOTIS trains merged into them by number. DB wins
/// where it has data: a MOTIS stop is dropped if the DB train's route omits the
/// station, or the station's board lists other trains of that class for the hour.
fn merge(stations: &[Station], boards: &Boards, raw: HashMap<String, Trip>) -> Vec<Trip> {
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
            route.extend(stations[idx].db_name.clone());
            for e in [&v.arrival, &v.departure].into_iter().flatten() {
                route.extend(e.path().iter().cloned());
            }
        }
        let mut labels: HashMap<String, usize> = HashMap::new();
        for (_, v) in calls.iter().filter(|(_, v)| v.category.is_some()) {
            *labels.entry(v.label()).or_default() += 1;
        }
        let label = labels
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map_or_else(|| calls[0].1.label(), |(l, _)| l);
        let mut numbers: Vec<u32> = calls.iter().filter_map(|(_, v)| v.number).collect();
        numbers.sort_unstable();
        numbers.dedup();
        trips.push(Trip {
            label,
            numbers,
            class: calls.iter().find_map(|(_, v)| v.class),
            headsign: calls
                .iter()
                .rev()
                .find_map(|(_, v)| v.departure.as_ref()?.path().last().cloned()),
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
                    from_db: true,
                })
                .collect(),
        });
        routes.push(route);
    }
    // Coupled trains (e.g. ICE 774 + ICE 2808) appear as separate runs with the
    // same timetable; while they run together they are one train for us.
    type Signature = Vec<(usize, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>;
    let mut seen: HashMap<Signature, usize> = HashMap::new();
    let mut kept: Vec<Trip> = Vec::new();
    let mut kept_routes: Vec<HashSet<String>> = Vec::new();
    for (trip, route) in trips.into_iter().zip(routes) {
        let signature: Signature = trip
            .calls
            .iter()
            .map(|c| (c.station, c.arrival.map(|t| t.planned), c.departure.map(|t| t.planned)))
            .collect();
        match seen.get(&signature) {
            Some(&i) => {
                let other = &mut kept[i];
                other.label = format!("{} + {}", other.label, trip.label);
                other.numbers.extend(trip.numbers);
                kept_routes[i].extend(route);
            }
            None => {
                seen.insert(signature, kept.len());
                kept.push(trip);
                kept_routes.push(route);
            }
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
    type Seen = (usize, Option<DateTime<Utc>>, Option<DateTime<Utc>>);
    let mut at_station: HashMap<usize, Vec<Seen>> = HashMap::new();
    let index = |at_station: &mut HashMap<usize, Vec<Seen>>, i: usize, calls: &[Call]| {
        for c in calls {
            let (arr, dep) = (c.arrival.map(|t| t.planned), c.departure.map(|t| t.planned));
            at_station.entry(c.station).or_default().push((i, arr, dep));
        }
    };
    let close = |a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>| {
        matches!((a, b), (Some(a), Some(b)) if (a - b).abs() <= SAME_TIME)
    };
    for (i, t) in trips.iter().enumerate() {
        index(&mut at_station, i, &t.calls);
    }

    let mut raw: Vec<(String, Trip)> = raw.into_iter().collect();
    raw.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, motis) in raw {
        let by_number = motis.numbers.iter().find_map(|n| {
            by_number.get(n)?.iter().copied().find(|&i| {
                motis.calls.iter().any(|c| {
                    trips[i].call_at(c.station).is_some_and(|d| match (d.planned(), c.planned()) {
                        (Some(a), Some(b)) => (a - b).abs() <= MERGE_TOLERANCE,
                        _ => false,
                    })
                })
            })
        });
        let twin = by_number.or_else(|| {
            let mut shared: HashMap<usize, usize> = HashMap::new();
            for c in &motis.calls {
                let (arr, dep) = (c.arrival.map(|t| t.planned), c.departure.map(|t| t.planned));
                for &(i, other_arr, other_dep) in at_station.get(&c.station).into_iter().flatten() {
                    if close(arr, other_arr) || close(dep, other_dep) {
                        *shared.entry(i).or_default() += 1;
                    }
                }
            }
            shared.into_iter().filter(|&(_, n)| n >= 2).max_by_key(|&(i, n)| (n, Reverse(i))).map(|(i, _)| i)
        });
        match twin {
            Some(i) => {
                let mut added = Vec::new();
                for call in motis.calls {
                    let off_route = stations[call.station]
                        .db_name
                        .as_ref()
                        .is_some_and(|n| !routes[i].is_empty() && !routes[i].contains(n));
                    if trips[i].call_at(call.station).is_none() && !off_route && !covered(&call, motis.class) {
                        added.push(call.clone());
                        trips[i].calls.push(call);
                    }
                }
                index(&mut at_station, i, &added);
            }
            None => {
                let class = motis.class;
                let calls: Vec<Call> = motis.calls.into_iter().filter(|c| !covered(c, class)).collect();
                index(&mut at_station, trips.len(), &calls);
                trips.push(Trip { calls, ..motis });
                routes.push(HashSet::new());
            }
        }
    }
    trips
}
