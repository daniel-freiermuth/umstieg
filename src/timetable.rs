//! A small timetable for the corridor of one search: trains from MOTIS rough
//! routes, overridden and extended by DB station boards where available.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use futures::future::join_all;

use crate::checked;
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
/// Walk between nearby stations without a MOTIS footpath: `SAME_STATION_METERS`
/// at walking pace (1 m/s) is at most this long.
const NEARBY_WALK: TimeDelta = TimeDelta::minutes(5);
/// DB boards cover German stations; elsewhere they are empty.
const DB_TIME_ZONE: &str = "Europe/Berlin";
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
    /// Whether Transitous or EFA knows its stops: it came from a rough route
    /// or one was merged into it. Otherwise only DB boards know it.
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
    /// Name in DB's data, as used in train paths.
    pub db_name: Option<String>,
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
    /// Stations added from DB train paths.
    pub discovered: Vec<String>,
    pub warnings: Vec<String>,
}

/// What is known about a station when it is first seen.
#[derive(Default, Clone, Copy)]
struct Sighting<'a> {
    name: &'a str,
    eva: Option<&'a str>,
    db_name: Option<&'a str>,
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
            if let Some(name) = &s.db_name {
                by_key.insert(format!("name:{}", db::fold(name)), i);
            }
            if let Some(eva) = &s.eva {
                by_key.insert(format!("eva:{eva}"), i);
            }
        }
        Self { list, by_key }
    }

    /// One index per physical station, unified by EVA number, folded name or
    /// proximity. Nearby stations with different EVA numbers stay separate
    /// (S-Bahn vs. long-distance station) and get a footpath instead.
    fn intern(&mut self, s: Sighting<'_>) -> usize {
        let name_key = format!("name:{}", db::fold(s.name));
        let eva_key = s.eva.map(|e| format!("eva:{e}"));
        let found =
            eva_key.as_ref().and_then(|k| self.by_key.get(k)).or_else(|| self.by_key.get(&name_key)).copied().or_else(
                || {
                    let here = s.position?;
                    self.list.iter().position(|st| {
                        let compatible = st.eva.is_none() || s.eva.is_none();
                        compatible && st.position.is_some_and(|p| meters(p, here) <= SAME_STATION_METERS)
                    })
                },
            );
        let idx = found.unwrap_or_else(|| {
            self.list.push(Station {
                name: s.name.to_owned(),
                eva: None,
                db_name: None,
                tz: None,
                position: None,
                motis_id: None,
            });
            self.list.len().saturating_sub(1)
        });
        self.by_key.insert(name_key, idx);
        if let Some(k) = eva_key {
            self.by_key.insert(k, idx);
        }
        // `idx` came from the list itself, so the station is always there.
        let Some(station) = self.list.get_mut(idx) else { return idx };
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
    Sighting { name, eva: db.map(|s| s.eva.as_str()), db_name: db.map(|s| s.name.as_str()), tz, position, motis_id }
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
    let trips = match db {
        Some(db) => {
            let boards = fetch_boards(motis, db, &mut stations, &rough.corridor, &mut discovered, &mut warnings).await;
            merge(&stations.list, &boards, rough.trips)
        }
        None => rough.trips.into_values().collect(),
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
fn propagate_delays(calls: &mut [Call]) {
    let mut delay = TimeDelta::zero();
    let mut previous: Option<DateTime<Utc>> = None;
    for call in calls {
        let from_db = call.from_db;
        for time in [&mut call.arrival, &mut call.departure].into_iter().flatten() {
            if from_db || time.actual != time.planned {
                delay = time.delay();
            } else {
                // Only absurd feed delays reach past representable time; such a
                // stop is then later than anything, i.e. unreachable.
                time.actual = checked::later_or_max(time.planned, delay.max(TimeDelta::zero()));
            }
            if let Some(previous) = previous.filter(|&p| time.actual < p) {
                time.actual = previous;
            }
            previous = Some(time.actual);
        }
    }
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
            from_db: false,
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
        boards.keys().filter_map(|&idx| Some((stations.list.get(idx)?.db_name.clone()?, idx))).collect();
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
    // unify, and trains there can be looked up.
    let lookups = between.iter().map(|(name, _)| async move {
        let station = db.resolve(name, None).await;
        let stop = motis.stop(name).await.ok().filter(|s| db::same_place(&s.name, name));
        (station, stop)
    });
    let mut wanted = Vec::new();
    for ((name, (_, lo, hi)), (result, stop)) in between.iter().zip(join_all(lookups).await) {
        match result {
            Ok(Some(station)) => {
                let idx = stations.intern(Sighting {
                    name: &station.name,
                    eva: Some(&station.eva),
                    db_name: Some(&station.name),
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

/// Whole seconds of a float, rounded and clamped to what a timetable needs.
#[expect(clippy::cast_possible_truncation, reason = "rounded and clamped to a range i64 holds")]
pub const fn whole_seconds(s: f64) -> i64 {
    s.round().clamp(-1e12, 1e12) as i64
}

/// A DB train's calls as (station, planned arrival, planned departure).
type Signature = Vec<(usize, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>;
/// A train seen at a station: (trip, planned arrival, planned departure).
type Seen = (usize, Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// DB trains from the boards, MOTIS trains merged into them by number. DB wins
/// where it has data: a MOTIS stop is dropped if the DB train's route omits the
/// station, or the station's board lists other trains of that class for the hour.
/// Without DB, copies of one train from several feeds become one, EFA's times winning.
#[expect(clippy::too_many_lines, reason = "the merge rules read best in one place, in order")]
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
            route.extend(stations.get(idx).and_then(|s| s.db_name.clone()));
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
                    from_db: true,
                })
                .collect(),
            transitous: false,
        });
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
            let db_times = trip.calls.iter().filter(|c| c.from_db).filter_map(Call::planned);
            let span = db_times.clone().min().zip(db_times.max());
            // Into another feed's copy, a stop is only taken where the copies
            // agree on the stops around it: Transitous' ICE 273 is 10 minutes
            // off at Karlsruhe, so its Mannheim time would be too.
            let from_feed = !trip.calls.iter().any(|c| c.from_db);
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
                let name = stations.get(call.station).and_then(|s| s.db_name.as_ref());
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
