#![expect(clippy::arithmetic_side_effects, reason = "fixture times")]

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use proptest::prelude::*;

use super::{
    Boards, Call, Footpath, Sighting, Station, Stations, Time, Trip, db_places, finish, footpaths, from_motis, hours,
    merge, meters, propagate_delays, whole_seconds,
};
use crate::db::{Coverage, Event, Visit};
use crate::motis::{Itinerary, Leg, Place, Stop};
use crate::testkit::{Builder, at, time};

// ---------------------------------------------------------------- helpers

/// (station, arrival, departure) per stop.
type Stops<'a> = [(usize, &'a str, &'a str)];
/// (name, calls as (from DB, arrival, departure), actual times in order)
type DelayCase<'a> = (&'a str, &'a [(bool, &'a str, &'a str)], &'a [&'a str]);
/// (name, MOTIS class, MOTIS calls, merged)
type SameTimeCase<'a> = (&'a str, Option<char>, &'a Stops<'a>, bool);
/// (name, MOTIS class, extra stop, its time, board coverage there, kept)
type StopCase<'a> = (&'a str, Option<char>, usize, &'a str, &'a [(DateTime<Utc>, char)], bool);

fn minutes(m: i64) -> TimeDelta {
    TimeDelta::minutes(m)
}

/// A station known to DB under `db_name` (`None`: only MOTIS knows it).
fn station(name: &str, db_name: Option<&str>) -> Station {
    Station {
        name: name.to_owned(),
        eva: None,
        db_name: db_name.map(str::to_owned),
        tz: None,
        position: None,
        motis_id: None,
    }
}

/// A DB event from a testkit time ("09:02+5"); "" is none.
fn event(s: &str) -> Option<Event> {
    let t = time(s)?;
    Some(Event { planned: Some(t.planned), changed: (t.actual != t.planned).then_some(t.actual), ..Event::default() })
}

/// One DB visit; `label` is "CATEGORY NUMBER" or a bare number.
fn visit(run: &str, seq: u32, label: &str, class: Option<char>, arrival: &str, departure: &str) -> Visit {
    let (category, number) =
        label.split_once(' ').map_or_else(|| (None, label.parse().ok()), |(c, n)| (Some(c.to_owned()), n.parse().ok()));
    Visit {
        trip: run.to_owned(),
        seq,
        number,
        class,
        category,
        line: None,
        arrival: event(arrival),
        departure: event(departure),
    }
}

/// A DB run calling at (station, arrival, departure), in sequence.
fn run(id: &str, label: &str, class: Option<char>, stops: &[(usize, &str, &str)]) -> Vec<(usize, Visit)> {
    (1..)
        .zip(stops)
        .map(|(seq, &(station, arrival, departure))| (station, visit(id, seq, label, class, arrival, departure)))
        .collect()
}

/// Sets the stations listed after the visit (its departure's path).
fn path_after(mut visits: Vec<(usize, Visit)>, station: usize, path: &[&str]) -> Vec<(usize, Visit)> {
    for (_, v) in visits.iter_mut().filter(|(s, _)| *s == station) {
        v.departure.as_mut().expect("visit departs").planned_path = path.iter().map(|&s| s.to_owned()).collect();
    }
    visits
}

/// Boards holding `visits` at their stations, covering nothing.
fn boards(visits: impl IntoIterator<Item = (usize, Visit)>) -> Boards {
    let mut out = Boards::new();
    for (station, v) in visits {
        out.entry(station).or_default().0.push(v);
    }
    out
}

fn cover(boards: &mut Boards, station: usize, entries: &[(DateTime<Utc>, char)]) {
    boards.entry(station).or_default().1 = Coverage::of(entries);
}

fn call(station: usize, from_db: bool, arrival: &str, departure: &str) -> Call {
    Call { station, arrival: time(arrival), departure: time(departure), platform: None, cancelled: false, from_db }
}

/// A MOTIS train; its numbers are the label's digits.
fn motis_trip(label: &str, class: Option<char>, stops: &[(usize, &str, &str)]) -> Trip {
    let digits: String = label.chars().filter(char::is_ascii_digit).collect();
    Trip {
        label: label.to_owned(),
        numbers: digits.parse().into_iter().collect(),
        class,
        headsign: None,
        calls: stops.iter().map(|&(s, a, d)| call(s, false, a, d)).collect(),
        transitous: true,
    }
}

fn raw(trips: Vec<(&str, Trip)>) -> HashMap<String, Trip> {
    trips.into_iter().map(|(id, t)| (id.to_owned(), t)).collect()
}

/// (station, from DB) per call.
fn calls_of(trip: &Trip) -> Vec<(usize, bool)> {
    trip.calls.iter().map(|c| (c.station, c.from_db)).collect()
}

fn labels(trips: &[Trip]) -> Vec<&str> {
    trips.iter().map(|t| t.label.as_str()).collect()
}

/// `p` moved `m` metres north.
fn north(p: (f64, f64), m: f64) -> (f64, f64) {
    (p.0 + m / 111_194.93, p.1)
}

fn place(name: &str, stop_id: Option<&str>, arrival: &str, departure: &str) -> Place {
    let (arrival, departure) = (time(arrival), time(departure));
    Place {
        name: name.to_owned(),
        stop_id: stop_id.map(str::to_owned),
        parent_id: None,
        tz: None,
        track: None,
        lat: None,
        lon: None,
        arrival: arrival.map(|t| t.actual),
        departure: departure.map(|t| t.actual),
        scheduled_arrival: arrival.map(|t| t.planned),
        scheduled_departure: departure.map(|t| t.planned),
        cancelled: false,
    }
}

/// A stop of a ride, with a stop id.
fn stop(name: &str, arrival: &str, departure: &str) -> Place {
    place(name, Some(name), arrival, departure)
}

fn leg(mode: &str, trip_id: Option<&str>, places: Vec<Place>, start: &str, end: &str) -> Leg {
    let mut places = places.into_iter();
    let from = places.next().expect("leg has a start");
    let mut intermediate_stops: Vec<Place> = places.collect();
    let to = intermediate_stops.pop().expect("leg has an end");
    Leg {
        mode: mode.to_owned(),
        from,
        to,
        start_time: at(start),
        end_time: at(end),
        display_name: None,
        route_short_name: None,
        trip_id: trip_id.map(str::to_owned),
        trip_short_name: None,
        headsign: None,
        agency_name: None,
        intermediate_stops,
    }
}

/// A train ride from the first stop's departure to the last stop's arrival.
fn ride(trip_id: &str, places: Vec<Place>) -> Leg {
    let start = places[0].scheduled_departure.expect("ride departs");
    let end = places.last().and_then(|p| p.scheduled_arrival).expect("ride arrives");
    let mut leg = leg("REGIONAL_RAIL", Some(trip_id), places, "00:00", "00:00");
    (leg.start_time, leg.end_time) = (start, end);
    leg
}

fn walk(from: Place, to: Place, start: &str, end: &str) -> Leg {
    leg("WALK", None, vec![from, to], start, end)
}

fn fixture_itinerary() -> Itinerary {
    serde_json::from_str(include_str!("../testdata/timetable/motis-itinerary.json")).expect("fixture parses")
}

/// MOTIS itineraries as `from_motis` sees them, with origin and destination
/// interned first like `build` does.
fn rough(itineraries: &[Itinerary], all_stops: bool) -> (Stations, super::Rough) {
    let mut stations = Stations::default();
    let origin = stations.intern(Sighting { name: "Origin", ..Sighting::default() });
    let destination = stations.intern(Sighting { name: "Destination", ..Sighting::default() });
    let rough = from_motis(itineraries, &HashMap::new(), &mut stations, (origin, destination), all_stops);
    (stations, rough)
}

fn index_of(stations: &Stations, name: &str) -> usize {
    stations.list.iter().position(|s| s.name == name).unwrap_or_else(|| panic!("no station {name}"))
}

// ---------------------------------------------------------------- times

#[test]
fn time_new_prefers_planned_and_falls_back_to_the_other() {
    let (p, a) = (at("08:00"), at("08:07"));
    let cases = [
        (Some(p), Some(a), Some(Time { planned: p, actual: a })),
        (Some(p), None, Some(Time { planned: p, actual: p })),
        (None, Some(a), Some(Time { planned: a, actual: a })),
        (None, None, None),
    ];
    for (planned, actual, expected) in cases {
        assert_eq!(Time::new(planned, actual), expected, "planned {planned:?}, actual {actual:?}");
    }
}

#[test]
fn call_times_use_actual_and_fall_back_to_departure_for_arrival() {
    // (arrival, departure) → (arrival_time, departure_time, planned)
    let cases = [
        ("09:00+3", "09:02+4", Some("09:03"), Some("09:06"), Some("09:00")),
        ("", "08:00+5", Some("08:05"), Some("08:05"), Some("08:00")),
        ("10:00-1", "", Some("09:59"), None, Some("10:00")),
        ("", "", None, None, None),
    ];
    for (arrival, departure, arrival_time, departure_time, planned) in cases {
        let c = call(0, true, arrival, departure);
        assert_eq!(c.arrival_time(), arrival_time.map(at), "arrival of ({arrival}, {departure})");
        assert_eq!(c.departure_time(), departure_time.map(at), "departure of ({arrival}, {departure})");
        assert_eq!(c.planned(), planned.map(at), "planned of ({arrival}, {departure})");
    }
}

#[test]
fn ignore_delays_restores_planned_times_but_keeps_cancellations() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00+5"), ("B", "09:00+7", "09:02-1"), ("C", "10:00+4", "")]);
    b.cancel(ice, "B");
    let mut tt = b.build("A", "C");
    tt.ignore_delays();
    let calls = &tt.trips[ice].calls;
    assert_eq!(calls.len(), 3);
    for c in calls {
        for t in [c.arrival, c.departure].into_iter().flatten() {
            assert_eq!(t.actual, t.planned);
        }
    }
    assert_eq!(calls.iter().map(|c| c.cancelled).collect::<Vec<_>>(), [false, true, false]);
    assert_eq!(calls[1].departure_time(), Some(at("09:02")));
}

#[test]
fn whole_seconds_rounds_half_away_and_clamps() {
    let cases = [
        (1.4, 1),
        (1.5, 2),
        (-1.5, -2),
        (299.6, 300),
        (1e20, 1_000_000_000_000),
        (f64::INFINITY, 1_000_000_000_000),
        (f64::NEG_INFINITY, -1_000_000_000_000),
        (f64::NAN, 0),
    ];
    for (input, expected) in cases {
        assert_eq!(whole_seconds(input), expected, "{input}");
    }
}

#[test]
fn meters_matches_known_great_circle_distances() {
    // (a, b, metres, tolerance)
    let cases = [
        ((50.0, 8.0), (51.0, 8.0), 111_195.0, 1.0),   // a degree of latitude
        ((0.0, 10.0), (0.0, 11.0), 111_195.0, 1.0),   // a degree of longitude at the equator
        ((60.0, 10.0), (60.0, 11.0), 55_597.0, 50.0), // half that at 60°
        ((48.8566, 2.3522), (51.5074, -0.1278), 343_560.0, 1000.0), // Paris – London
        ((49.4794, 8.4689), (49.4794, 8.4689), 0.0, 1e-6),
    ];
    for (a, b, expected, tolerance) in cases {
        for (x, y) in [(a, b), (b, a)] {
            let d = meters(x, y);
            assert!((d - expected).abs() <= tolerance, "{x:?} → {y:?}: {d} m, expected {expected}");
        }
    }
}

// ---------------------------------------------------------------- delays

#[test]
fn propagate_delays_carries_the_last_known_delay_forward() {
    let cases: [DelayCase<'_>; 7] = [
        (
            "DB delay carried over MOTIS stops without live data",
            &[(true, "", "08:00+5"), (false, "09:00", "09:02"), (true, "10:00+2", "")],
            &["08:05", "09:05", "09:07", "10:02"],
        ),
        (
            "MOTIS live data replaces the carried delay",
            &[(true, "", "08:00+5"), (false, "09:00+1", "09:02"), (false, "10:00", "")],
            &["08:05", "09:01", "09:03", "10:01"],
        ),
        (
            "an early train is not assumed early further on",
            &[(true, "", "08:00-3"), (false, "09:00", "")],
            &["07:57", "09:00"],
        ),
        (
            "a delay shrinks by at most a quarter of each stretch, DB's own times included",
            &[(true, "", "08:00+30"), (true, "08:20", "08:22"), (true, "09:00", "")],
            // 20 minutes take at least 15; 38 at least 29 (whole minutes made up).
            &["08:30", "08:45", "08:45", "09:14"],
        ),
        (
            "no forecast abroad: IC 5762 leaves Padborg 34 late, 'on time' in Kolding",
            &[(true, "", "16:10+34"), (true, "17:05", "")],
            &["16:44", "17:26"],
        ),
        (
            "a dwell time may shrink to nothing: the delay is made up there",
            &[(true, "", "08:00+5"), (true, "08:30+5", "08:40"), (true, "09:00", "")],
            &["08:05", "08:35", "08:40", "09:00"],
        ),
        ("no live data anywhere: planned", &[(false, "", "08:00"), (false, "09:00", "")], &["08:00", "09:00"]),
    ];
    for (name, stops, expected) in cases {
        let mut calls: Vec<Call> = stops.iter().map(|&(db, a, d)| call(0, db, a, d)).collect();
        let planned: Vec<_> =
            calls.iter().flat_map(|c| [c.arrival, c.departure]).flatten().map(|t| t.planned).collect();
        propagate_delays(&mut calls);
        let times: Vec<_> = calls.iter().flat_map(|c| [c.arrival, c.departure]).flatten().collect();
        let expected: Vec<_> = expected.iter().map(|&t| at(t)).collect();
        assert_eq!(times.iter().map(|t| t.actual).collect::<Vec<_>>(), expected, "{name}");
        assert_eq!(times.iter().map(|t| t.planned).collect::<Vec<_>>(), planned, "{name}: planned untouched");
    }
}

#[test]
fn absurd_delays_make_later_stops_unreachable_instead_of_panicking() {
    let (min, max) = (DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC);
    let mut calls = vec![
        Call {
            station: 0,
            arrival: None,
            departure: Some(Time { planned: min, actual: max }),
            platform: None,
            cancelled: false,
            from_db: true,
        },
        call(1, false, "09:00", ""),
    ];
    propagate_delays(&mut calls);
    assert_eq!(calls[1].arrival.map(|t| t.actual), Some(max));
}

#[test]
fn board_hours_near_the_ends_of_time_stay_in_range() {
    // First the plain case: it fails fast where the open-ended ones below would loop for ages.
    assert_eq!(hours(at("08:10"), at("08:50")), [at("07:00"), at("08:00"), at("09:00")]);
    let (min, max) = (DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC);
    let none: Vec<DateTime<Utc>> = Vec::new();
    assert_eq!(hours(min, min), none, "no hour before the earliest time");
    // chrono can't round times past 2262 to the hour: no boards asked for, no panic.
    assert_eq!(hours(max - TimeDelta::minutes(10), max), none);
    let far = hours(at("08:10"), max);
    assert_eq!((far.len(), far.first()), (10, Some(&at("07:00"))), "capped, the end saturates");
}

/// (from DB, arrival?, departure?, minutes since previous stop, dwell, arrival delay, departure delay)
type Stop7 = (bool, bool, bool, i64, i64, Option<i64>, Option<i64>);

proptest! {
    /// Live times (DB's, or MOTIS's with a delay) keep their value unless an
    /// earlier time is later; other times get the last live delay if positive;
    /// the result never goes back in time and planned times stay.
    #[test]
    fn propagate_delays_rules(stops in prop::collection::vec(
        (any::<bool>(), any::<bool>(), any::<bool>(), 0..30i64, 0..5i64,
         prop::option::of(-5..40i64), prop::option::of(-5..40i64)),
        1..12,
    )) {
        let stops: Vec<Stop7> = stops;
        let mut t = at("08:00");
        let mut calls = Vec::new();
        for (from_db, has_arrival, has_departure, gap, dwell, arrival_delay, departure_delay) in stops {
            t += minutes(gap);
            let arrival = has_arrival.then(|| Time { planned: t, actual: t + minutes(arrival_delay.unwrap_or(0)) });
            t += minutes(dwell);
            let departure =
                has_departure.then(|| Time { planned: t, actual: t + minutes(departure_delay.unwrap_or(0)) });
            calls.push(Call { station: 0, arrival, departure, platform: None, cancelled: false, from_db });
        }
        let before: Vec<(bool, Time)> = calls
            .iter()
            .flat_map(|c| [c.arrival, c.departure].into_iter().flatten().map(move |t| (c.from_db, t)))
            .collect();
        propagate_delays(&mut calls);
        prop_assert_eq!(before.len(), calls.iter().flat_map(|c| [c.arrival, c.departure]).flatten().count());
        // The model: per call, its first time is at least the last time at the
        // call before plus the stretch's planned time less a quarter (whole
        // minutes); no time is earlier than the one before.
        let mut delay = TimeDelta::zero();
        let mut previous: Option<Time> = None;
        let mut left: Option<Time> = None;
        let mut k = 0;
        for c in &calls {
            let mut first = true;
            for new in [c.arrival, c.departure].into_iter().flatten() {
                let (from_db, old) = before[k];
                k += 1;
                prop_assert_eq!(new.planned, old.planned);
                let live = from_db || old.actual != old.planned;
                let mut expected = if live { old.actual } else { old.planned + delay.max(TimeDelta::zero()) };
                if let Some(l) = left.filter(|_| first) {
                    let planned = old.planned - l.planned;
                    let fastest = planned - minutes(planned.num_minutes().div_euclid(4));
                    expected = expected.max(l.actual + fastest);
                }
                if let Some(p) = previous {
                    expected = expected.max(p.actual);
                }
                prop_assert_eq!(new.actual, expected);
                delay = new.delay();
                previous = Some(new);
                first = false;
            }
            left = previous;
        }
    }
}

#[test]
fn finish_drops_single_calls_and_orders_calls_by_planned_time() {
    let merged = Trip {
        label: "ICE 378".to_owned(),
        numbers: vec![378],
        class: Some('F'),
        headsign: None,
        // DB calls first, MOTIS calls appended by the merge.
        calls: vec![
            call(1, true, "07:15+10", "07:17+10"),
            call(2, true, "08:21+10", ""),
            call(0, false, "", "06:39"),
            call(3, false, "07:40", "07:41"),
        ],
        transitous: true,
    };
    let lonely = motis_trip("Bus 1", None, &[(5, "", "08:00")]);
    let trips = finish(vec![lonely, merged]);
    assert_eq!(labels(&trips), ["ICE 378"]);
    let calls = &trips[0].calls;
    assert_eq!(calls.iter().map(|c| c.station).collect::<Vec<_>>(), [0, 1, 3, 2]);
    // Hanau, from MOTIS, now lies between the delayed DB stops.
    assert_eq!(calls[2].arrival_time(), Some(at("07:50")));
}

// ---------------------------------------------------------------- stations

#[test]
fn stations_unify_by_folded_name() {
    let cases = [
        ("København H", "Koebenhavn H", true),
        ("Frankfurt (Main) Hbf", "frankfurt main  hbf", true),
        ("Hamburg Hbf", "Hamburg-Altona", false),
    ];
    for (first, second, same) in cases {
        let mut stations = Stations::default();
        let a = stations.intern(Sighting { name: first, ..Sighting::default() });
        let b = stations.intern(Sighting { name: second, ..Sighting::default() });
        assert_eq!(a == b, same, "{first} / {second}");
        assert_eq!(stations.list[a].name, first, "the first name stays");
    }
}

#[test]
fn stations_unify_by_eva_across_names() {
    let mut stations = Stations::default();
    let motis = stations.intern(Sighting {
        name: "Frankfurt (Main) Flughafen Fernbahnhof",
        eva: Some("8070003"),
        ..Sighting::default()
    });
    let db = stations.intern(Sighting {
        name: "Frankfurt(M) Flughafen Fernbf",
        eva: Some("8070003"),
        db_name: Some("Frankfurt(M) Flughafen Fernbf"),
        ..Sighting::default()
    });
    // DB's name is now known too, even without an EVA number.
    let again = stations.intern(Sighting { name: "Frankfurt(M) Flughafen Fernbf", ..Sighting::default() });
    assert_eq!((db, again), (motis, motis));
    assert_eq!(stations.list.len(), 1);
    assert_eq!(stations.list[0].db_name.as_deref(), Some("Frankfurt(M) Flughafen Fernbf"));
}

#[test]
fn stations_unify_by_proximity_unless_both_have_different_evas() {
    let altona = (53.551_853, 9.934_554);
    // (first eva, second eva, distance in metres, same station)
    let cases = [
        (None, None, 78.0, true),
        (None, None, 299.0, true),
        (None, None, 301.0, false),
        (Some("8002553"), None, 200.0, true),
        (None, Some("8002553"), 200.0, true),
        (Some("8002553"), Some("8002553"), 200.0, true),
        (Some("8002553"), Some("8098553"), 50.0, false),
    ];
    for (eva_a, eva_b, distance, same) in cases {
        let mut stations = Stations::default();
        let a = stations.intern(Sighting { name: "Altona", eva: eva_a, position: Some(altona), ..Sighting::default() });
        let b = stations.intern(Sighting {
            name: "Hamburg-Altona",
            eva: eva_b,
            position: Some(north(altona, distance)),
            ..Sighting::default()
        });
        assert_eq!(a == b, same, "{eva_a:?} / {eva_b:?} at {distance} m");
    }
    // Without a position on both sides there is nothing to compare.
    let mut stations = Stations::default();
    let a = stations.intern(Sighting { name: "Altona", position: Some(altona), ..Sighting::default() });
    let b = stations.intern(Sighting { name: "Hamburg-Altona", ..Sighting::default() });
    assert_ne!(a, b);
}

#[test]
fn stations_fill_unknown_fields_from_later_sightings_first_wins() {
    let mut stations = Stations::default();
    let a = stations.intern(Sighting { name: "Fulda", ..Sighting::default() });
    stations.intern(Sighting {
        name: "Fulda",
        eva: Some("8000115"),
        db_name: Some("Fulda"),
        tz: Some("Europe/Berlin"),
        position: Some((50.554, 9.684)),
        motis_id: Some("de-DELFI_de:06631:1234_G"),
    });
    stations.intern(Sighting {
        name: "Fulda",
        eva: Some("1234567"),
        db_name: Some("Fulda Bf"),
        tz: Some("Europe/Paris"),
        position: Some((1.0, 1.0)),
        motis_id: Some("other"),
    });
    let s = &stations.list[a];
    assert_eq!(stations.list.len(), 1);
    assert_eq!(
        (s.eva.as_deref(), s.db_name.as_deref(), s.tz.as_deref(), s.position, s.motis_id.as_deref()),
        (
            Some("8000115"),
            Some("Fulda"),
            Some("Europe/Berlin"),
            Some((50.554, 9.684)),
            Some("de-DELFI_de:06631:1234_G")
        )
    );
}

#[test]
fn nearby_stations_kept_apart_get_minimum_walks_both_ways() {
    let hbf = (53.552_734, 10.006_909);
    let positioned = |name: &str, eva: &str, position| Station {
        eva: Some(eva.to_owned()),
        position: Some(position),
        ..station(name, Some(name))
    };
    let stations = [
        positioned("Hamburg Hbf", "8002549", hbf),
        positioned("Hamburg Hbf (S-Bahn)", "8098549", north(hbf, 120.0)),
        positioned("Hamburg Hbf (U)", "1", north(hbf, 299.0)),
        positioned("Hamburg Dammtor", "8002548", north(hbf, 1500.0)),
        station("Somewhere", None),
    ];
    // MOTIS knows a quicker way from the S-Bahn station up to the main hall.
    let motis = HashMap::from([((1, 0), minutes(2))]);
    let got: Vec<(usize, usize, TimeDelta)> =
        footpaths(&stations, motis).into_iter().map(|Footpath { from, to, duration }| (from, to, duration)).collect();
    // 299 m at 1 m/s is just under the 5 minute minimum.
    let expected = [
        (0, 1, minutes(5)),
        (0, 2, minutes(5)),
        (1, 0, minutes(2)),
        (1, 2, minutes(5)),
        (2, 0, minutes(5)),
        (2, 1, minutes(5)),
    ];
    assert_eq!(got, expected);
}

// ---------------------------------------------------------------- MOTIS legs

#[test]
fn db_places_are_rail_stops_in_germany_plus_the_ends() {
    let mut copenhagen = stop("København H", "15:38", "");
    copenhagen.tz = Some("Europe/Copenhagen".to_owned());
    let mut hamburg = stop("Hamburg Hbf", "10:28", "10:36");
    hamburg.tz = Some("Europe/Berlin".to_owned());
    let itinerary = Itinerary {
        legs: vec![
            walk(place("START", None, "", "04:28"), stop("Bad Dürkheim", "04:29", ""), "04:28", "04:29"),
            leg(
                "BUS",
                Some("bus"),
                vec![stop("Bad Dürkheim", "", "04:29"), stop("Ring", "04:40", "")],
                "04:29",
                "04:40",
            ),
            ride("ice", vec![stop("Mannheim Hbf", "", "06:03"), hamburg, stop("Flensburg", "12:00", "")]),
            ride("rj", vec![stop("Flensburg", "", "12:10"), copenhagen]),
        ],
    };
    let origin = Stop { id: "o-id".to_owned(), name: "Origin".to_owned(), position: None };
    let destination = Stop { id: "d-id".to_owned(), name: "Destination".to_owned(), position: None };
    let places = db_places(std::slice::from_ref(&itinerary), &origin, &destination);
    let expected: HashMap<&str, Option<&str>> = HashMap::from([
        ("Origin", Some("o-id")),
        ("Destination", Some("d-id")),
        ("Mannheim Hbf", Some("Mannheim Hbf")),
        ("Hamburg Hbf", Some("Hamburg Hbf")),
        ("Flensburg", Some("Flensburg")),
    ]);
    assert_eq!(places, expected);
}

#[test]
fn motis_itinerary_becomes_trips_keyed_by_trip_id() {
    let (stations, rough) = rough(&[fixture_itinerary()], false);
    let ids: HashSet<&str> = rough.trips.keys().map(String::as_str).collect();
    assert_eq!(
        ids,
        HashSet::from([
            "20261024_06:14_de-DELFI_3448990207",
            "20261024_07:06_de-DELFI_3449008863",
            "20261024_07:23_de-DELFI_3448677552",
            "20261024_12:29_de-DELFI_3449055858",
        ])
    );
    let ice = &rough.trips["20261024_07:23_de-DELFI_3448677552"];
    assert_eq!((ice.label.as_str(), ice.numbers.as_slice(), ice.class), ("ICE 774", &[774][..], Some('F')));
    assert_eq!(ice.headsign.as_deref(), Some("Kiel Hbf"));
    let names: Vec<&str> = ice.calls.iter().map(|c| stations.list[c.station].name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Mannheim, Hauptbahnhof",
            "Frankfurt (Main) Südbahnhof",
            "Kassel Bahnhof Wilhelmshöhe",
            "Göttingen",
            "Hamburg Hbf"
        ]
    );
    let first = &ice.calls[0];
    let last = &ice.calls[4];
    assert_eq!((first.arrival, first.departure.map(|t| t.planned)), (None, Some(at("06:03"))));
    assert_eq!(first.platform.as_deref(), Some("2"));
    assert_eq!((last.arrival, last.departure), (time("10:28+8"), None));
    assert_eq!(ice.calls[1].departure, time("06:39+12"));
    assert!(ice.calls.iter().all(|c| !c.from_db));

    let s4 = &rough.trips["20261024_07:06_de-DELFI_3449008863"];
    assert_eq!((s4.label.as_str(), s4.class), ("S4 (38415)", Some('S')));

    // Changes on one platform name (two Böbig stop ids, two Mannheim ones) and
    // the walk to the nearby Danish stop id of Altona need no footpath; the
    // only one is from the queried start coordinate to the first station.
    let duerkheim = index_of(&stations, "Bad Dürkheim, Bahnhof");
    assert_eq!(rough.footpaths, HashMap::from([((0, duerkheim), minutes(1))]));
    assert!(stations.list.iter().all(|s| s.name != "Hamburg-Altona"));
    assert_eq!(stations.list.iter().filter(|s| s.name == "Neustadt, Böbig").count(), 1);
}

#[test]
fn corridor_is_ride_ends_or_all_non_s_bahn_stops() {
    let itinerary = fixture_itinerary();
    let names = |all_stops| {
        let (stations, rough) = rough(std::slice::from_ref(&itinerary), all_stops);
        let mut names: Vec<String> = rough.corridor.keys().map(|&i| stations.list[i].name.clone()).collect();
        names.sort();
        (names, rough.corridor.into_iter().map(|(i, r)| (stations.list[i].name.clone(), r)).collect::<HashMap<_, _>>())
    };
    let (ends, ranges) = names(false);
    assert_eq!(ends, ["Altona", "Bad Dürkheim, Bahnhof", "Hamburg Hbf", "Mannheim, Hauptbahnhof", "Neustadt, Böbig"]);
    // Planned times, from the train arriving to the one leaving.
    assert_eq!(ranges["Hamburg Hbf"], (at("10:28"), at("10:42")));
    assert_eq!(ranges["Neustadt, Böbig"], (at("04:51"), at("05:08")));
    assert_eq!(ranges["Mannheim, Hauptbahnhof"], (at("05:48"), at("06:03")));

    let (all, _) = names(true);
    let extra: Vec<&String> = all.iter().filter(|n| !ends.contains(n)).collect();
    assert_eq!(
        extra,
        [
            "Deidesheim, Bahnhof",
            "Frankfurt (Main) Südbahnhof",
            "Göttingen",
            "Kassel Bahnhof Wilhelmshöhe",
            "Mußbach, Bahnhof",
            "Wachenheim/Wstr, Bahnhof",
        ]
    );
}

#[test]
fn stops_without_times_are_skipped() {
    let mut itinerary = fixture_itinerary();
    let ice = itinerary.legs.iter_mut().find(|l| l.mode == "HIGHSPEED_RAIL").expect("fixture has the ICE");
    let sued = &mut ice.intermediate_stops[0];
    assert_eq!(sued.name, "Frankfurt (Main) Südbahnhof");
    (sued.arrival, sued.departure, sued.scheduled_arrival, sued.scheduled_departure) = (None, None, None, None);
    let (stations, rough) = rough(&[itinerary], true);
    let ice = &rough.trips["20261024_07:23_de-DELFI_3448677552"];
    assert_eq!(ice.calls.len(), 4);
    let sued = index_of(&stations, "Frankfurt (Main) Südbahnhof");
    assert!(ice.call_at(sued).is_none());
    assert!(!rough.corridor.contains_key(&sued));
}
#[test]
fn rides_have_no_arrival_where_boarded_and_no_departure_where_left() {
    // MOTIS gives every stop its times, also arriving where we board.
    let legs =
        vec![ride("t", vec![stop("A", "07:58", "08:00"), stop("B", "08:30", "08:32"), stop("C", "09:00", "09:02")])];
    let (_, rough) = rough(&[Itinerary { legs }], false);
    let times: Vec<_> = rough.trips["t"].calls.iter().map(|c| (c.arrival, c.departure)).collect();
    assert_eq!(times, [(None, time("08:00")), (time("08:30"), time("08:32")), (time("09:00"), None)]);
}

#[test]
fn boarding_after_a_stop_without_times_needs_no_walk() {
    let places = vec![stop("X", "", ""), stop("A", "", "08:00"), stop("B", "08:30", "")];
    let legs = vec![leg("REGIONAL_RAIL", Some("t"), places, "08:00", "08:30")];
    let (stations, rough) = rough(&[Itinerary { legs }], false);
    let names: Vec<&str> = rough.trips["t"].calls.iter().map(|c| stations.list[c.station].name.as_str()).collect();
    assert_eq!(names, ["A", "B"]);
    assert_eq!(rough.footpaths, HashMap::new(), "no walk from X: the itinerary starts with the ride");
}

#[test]
fn walks_become_footpaths_between_their_stations() {
    let itineraries = [
        // Ride, two walks in a row, bus, walk to the queried destination.
        Itinerary {
            legs: vec![
                ride("t1", vec![stop("A", "", "08:00"), stop("B", "08:30", "")]),
                walk(stop("B", "08:30", ""), stop("B2", "08:33", ""), "08:30", "08:33"),
                walk(stop("B2", "08:33", ""), stop("C", "08:35", ""), "08:33", "08:35"),
                leg("BUS", Some("t2"), vec![stop("C", "", "08:45"), stop("D", "09:00", "")], "08:45", "09:00"),
                walk(stop("D", "09:00", ""), place("END", None, "09:04", ""), "09:00", "09:04"),
            ],
        },
        // The same change with a shorter walk.
        Itinerary {
            legs: vec![
                ride("t3", vec![stop("A", "", "09:00"), stop("B", "09:30", "")]),
                walk(stop("B", "09:30", ""), stop("C", "09:31", ""), "09:30", "09:31"),
                ride("t4", vec![stop("C", "", "09:40"), stop("D", "10:00", "")]),
            ],
        },
        // From the queried start coordinate.
        Itinerary {
            legs: vec![
                walk(place("START", None, "", "07:54"), stop("A", "08:00", ""), "07:54", "08:00"),
                ride("t5", vec![stop("A", "", "08:00"), stop("B", "08:30", "")]),
            ],
        },
        // From a hub stop (a half route starting there).
        Itinerary {
            legs: vec![
                walk(stop("Hub", "", "07:58"), stop("A", "08:00", ""), "07:58", "08:00"),
                ride("t6", vec![stop("A", "", "08:00"), stop("B", "08:30", "")]),
                walk(stop("B", "08:30", ""), stop("Hub2", "08:37", ""), "08:30", "08:37"),
            ],
        },
    ];
    let (stations, rough) = rough(&itineraries, false);
    let s = |name| index_of(&stations, name);
    let expected = HashMap::from([
        ((s("B"), s("C")), minutes(1)),
        ((s("D"), 1), minutes(4)),
        ((0, s("A")), minutes(6)),
        ((s("Hub"), s("A")), minutes(2)),
        ((s("B"), s("Hub2")), minutes(7)),
    ]);
    assert_eq!(rough.footpaths, expected);
    // Buses don't make corridor stations.
    let corridor: HashSet<usize> = rough.corridor.keys().copied().collect();
    assert_eq!(corridor, HashSet::from([s("A"), s("B"), s("C"), s("D")]));
    assert_eq!(rough.corridor[&s("C")], (at("09:40"), at("09:40")));
}

#[test]
fn legs_of_one_trip_from_different_itineraries_unite() {
    // The RJ ridden to a hub in one itinerary and on from there in another.
    let mut itineraries = [
        Itinerary { legs: vec![ride("rj", vec![stop("A", "", "08:00"), stop("B", "09:00", "")])] },
        Itinerary { legs: vec![ride("rj", vec![stop("B", "", "09:05"), stop("C", "10:00", "")])] },
    ];
    itineraries[1].legs[0].from.track = Some("2".to_owned());
    let (stations, rough) = rough(&itineraries, false);
    assert_eq!(rough.trips.len(), 1);
    let rj = &rough.trips["rj"];
    let names: Vec<&str> = rj.calls.iter().map(|c| stations.list[c.station].name.as_str()).collect();
    assert_eq!(names, ["A", "B", "C"]);
    let b = &rj.calls[1];
    assert_eq!((b.arrival, b.departure, b.platform.as_deref()), (time("09:00"), time("09:05"), Some("2")));
}

// ---------------------------------------------------------------- merge: DB runs

#[test]
fn db_run_joins_visits_across_boards_in_sequence_order() {
    let stations: Vec<Station> = ["Mannheim Hbf", "Frankfurt(M) Süd", "Fulda", "Kassel-Wilhelmshöhe", "Hamburg Hbf"]
        .iter()
        .map(|&n| station(n, Some(n)))
        .collect();
    // Station indices deliberately out of route order.
    let mut visits = vec![
        (4, visit("r1", 1, "ICE 378", Some('F'), "", "06:39+1")),
        (0, visit("r1", 2, "ICE 378", Some('F'), "07:15+2", "07:17+2")),
        (3, visit("r1", 3, "ICE 378", Some('F'), "08:21", "08:23")),
        (2, visit("r1", 4, "ICE 378", Some('F'), "09:30", "")),
        (1, visit("r2", 1, "RE 7", Some('N'), "", "07:00")),
        (3, visit("r2", 2, "RE 7", Some('N'), "07:30", "")),
    ];
    {
        let (_, frankfurt) = &mut visits[1];
        let arrival = frankfurt.arrival.as_mut().expect("arrives");
        arrival.planned_platform = Some("5".to_owned());
        let departure = frankfurt.departure.as_mut().expect("departs");
        departure.planned_platform = Some("5".to_owned());
        departure.changed_platform = Some("6".to_owned());
        departure.planned_path = vec!["Fulda".to_owned(), "Kassel-Wilhelmshöhe".to_owned(), "Hamburg Hbf".to_owned()];
        let (_, fulda) = &mut visits[2];
        fulda.departure.as_mut().expect("departs").planned_path = vec!["Kiel Hbf".to_owned()];
        let (_, last) = &mut visits[3];
        last.arrival.as_mut().expect("arrives").cancelled = true;
        last.arrival.as_mut().expect("arrives").planned_platform = Some("12".to_owned());
    }
    let trips = merge(&stations, &boards(visits), HashMap::new());
    assert_eq!(labels(&trips), ["ICE 378", "RE 7"]);
    let ice = &trips[0];
    assert_eq!(calls_of(ice), [(4, true), (0, true), (3, true), (2, true)]);
    assert_eq!((ice.numbers.as_slice(), ice.class), (&[378][..], Some('F')));
    // The last stop with a departure names the destination.
    assert_eq!(ice.headsign.as_deref(), Some("Kiel Hbf"));
    assert_eq!(ice.calls[1].arrival, time("07:15+2"));
    assert_eq!(ice.calls[1].departure, time("07:17+2"));
    // The departure's platform, its change first; else the arrival's.
    assert_eq!(
        ice.calls.iter().map(|c| c.platform.as_deref()).collect::<Vec<_>>(),
        [None, Some("6"), None, Some("12")]
    );
    assert_eq!(ice.calls.iter().map(|c| c.cancelled).collect::<Vec<_>>(), [false, false, false, true]);
}

#[test]
fn db_run_label_is_the_majority_label() {
    // (labels in sequence, expected label, expected numbers)
    let cases: [(&[&str], &str, &[u32]); 4] = [
        (&["ICE 2558", "ICE 76", "ICE 76"], "ICE 76", &[76, 2558]),
        (&["ICE 76", "ICE 2558"], "ICE 2558", &[76, 2558]),
        // Visits without a category don't vote.
        (&["5", "5", "IC 5"], "IC 5", &[5]),
        (&["7", "8"], "7", &[7, 8]),
    ];
    for (labels_in_order, expected, numbers) in cases {
        let stations: Vec<Station> = (0..labels_in_order.len()).map(|i| station(&i.to_string(), None)).collect();
        let visits = (0..).zip(labels_in_order).map(|(i, &label)| {
            let t = format!("{:02}:00", 8 + i);
            (i as usize, visit("r", i + 1, label, Some('F'), &t, &t))
        });
        let trips = merge(&stations, &boards(visits), HashMap::new());
        assert_eq!(trips.len(), 1);
        assert_eq!((trips[0].label.as_str(), trips[0].numbers.as_slice()), (expected, numbers), "{labels_in_order:?}");
    }
}

#[test]
fn coupled_db_runs_with_identical_calls_become_one_train() {
    let stations: Vec<Station> = ["A", "B", "C"].iter().map(|&n| station(n, Some(n))).collect();
    let ice_774 = || run("1-774", "ICE 774", Some('F'), &[(0, "", "08:00"), (1, "09:00+5", "")]);
    // (second run's calls, expected labels)
    let cases: [(&Stops<'_>, &[&str]); 4] = [
        (&[(0, "", "08:00"), (1, "09:00", "")], &["ICE 774 + ICE 2808"]),
        (&[(0, "", "08:00+3"), (1, "09:00+1", "")], &["ICE 774 + ICE 2808"]),
        (&[(0, "", "08:00"), (1, "09:01", "")], &["ICE 774", "ICE 2808"]),
        (&[(0, "", "08:00"), (1, "09:00", "09:02"), (2, "10:00", "")], &["ICE 774", "ICE 2808"]),
    ];
    for (second, expected) in cases {
        let visits = ice_774().into_iter().chain(run("2-2808", "ICE 2808", Some('F'), second));
        let trips = merge(&stations, &boards(visits), HashMap::new());
        assert_eq!(labels(&trips), expected, "{second:?}");
        if let [one] = trips.as_slice() {
            assert_eq!(one.numbers, [774, 2808]);
            // The first run's live data stays.
            assert_eq!(one.calls[1].arrival, time("09:00+5"));
        }
    }
}

#[test]
fn coupled_runs_share_their_routes_and_numbers() {
    // MOTIS knows only ICE 2808 and its stop at D, which only that run's path lists.
    let stations: Vec<Station> = ["A", "B", "D"].iter().map(|&n| station(n, Some(n))).collect();
    let visits = run("1-774", "ICE 774", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:02")])
        .into_iter()
        .chain(path_after(run("2-2808", "ICE 2808", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:02")]), 1, &["D"]));
    let motis = motis_trip("ICE 2808", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:02"), (2, "10:00", "")]);
    let trips = merge(&stations, &boards(visits), raw(vec![("m", motis)]));
    assert_eq!(labels(&trips), ["ICE 774 + ICE 2808"]);
    assert_eq!(calls_of(&trips[0]), [(0, true), (1, true), (2, false)]);
}

// ---------------------------------------------------------------- merge: MOTIS into DB

/// DB's ICE 616 at A and B; MOTIS has it at A and at E (unknown to DB, after
/// DB's last stop) shifted by `offset` minutes, under `label`.
fn merge_by_number(label: &str, offset: i64) -> Vec<Trip> {
    let stations = vec![station("A", Some("A")), station("B", Some("B")), station("E", None)];
    let visits = run("r", "ICE 616", Some('F'), &[(0, "", "08:00"), (1, "09:00", "")]);
    let [start, end] = [at("08:00"), at("11:00")].map(|t| (t + minutes(offset)).format("%H:%M").to_string());
    let motis = motis_trip(label, Some('F'), &[(0, "", start.as_str()), (2, end.as_str(), "")]);
    merge(&stations, &boards(visits), raw(vec![("m", motis)]))
}

#[test]
fn motis_merges_by_train_number_within_an_hour() {
    // (MOTIS label, offset in minutes, merged)
    let cases = [
        ("ICE 616", 0, true),
        ("ICE 616", 60, true),
        ("ICE 616", -60, true),
        ("ICE 616", 61, false),
        ("ICE 616", -61, false),
        ("ICE 617", 0, false),
    ];
    for (label, offset, merged) in cases {
        let trips = merge_by_number(label, offset);
        if merged {
            assert_eq!(labels(&trips), ["ICE 616"], "{label} {offset:+}");
            // DB's call at A stays; MOTIS adds E.
            assert_eq!(calls_of(&trips[0]), [(0, true), (1, true), (2, false)], "{label} {offset:+}");
        } else {
            assert_eq!(labels(&trips), ["ICE 616", label], "{label} {offset:+}");
            assert_eq!(calls_of(&trips[1]), [(0, false), (2, false)], "{label} {offset:+}");
        }
    }
}

#[test]
fn motis_merges_by_identical_times_at_two_stations_of_the_same_class() {
    // DB's ICE 76: A 08:00, B 09:00/09:05, C 10:00. MOTIS calls it ICE 2558
    // and also stops at E (unknown to DB) at 11:00.
    let stations = vec![station("A", Some("A")), station("B", Some("B")), station("C", Some("C")), station("E", None)];
    let visits = || run("r", "ICE 76", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:05"), (2, "10:00", "")]);
    let cases: [SameTimeCase<'_>; 7] = [
        ("two stations, 2 min off", Some('F'), &[(0, "", "08:00"), (1, "09:02", "")], true),
        ("departures compared too", Some('F'), &[(0, "", "07:58"), (1, "", "09:07")], true),
        ("3 min off at one of them", Some('F'), &[(0, "", "08:00"), (1, "09:03", "")], false),
        ("one station only", Some('F'), &[(0, "", "08:00")], false),
        ("S-Bahn on an ICE's times", Some('S'), &[(0, "", "08:00"), (1, "09:00", "")], false),
        ("regional train on an ICE's times", Some('N'), &[(0, "", "08:00"), (1, "09:00", ""), (2, "10:00", "")], false),
        ("foreign train without class", None, &[(0, "", "08:00"), (1, "09:00", "")], true),
    ];
    for (name, class, stops, merged) in cases {
        let mut calls = stops.to_vec();
        calls.push((3, "11:00", ""));
        let motis = motis_trip("ICE 2558", class, &calls);
        let trips = merge(&stations, &boards(visits()), raw(vec![("m", motis)]));
        if merged {
            assert_eq!(labels(&trips), ["ICE 76"], "{name}");
            assert_eq!(calls_of(&trips[0]), [(0, true), (1, true), (2, true), (3, false)], "{name}");
        } else {
            assert_eq!(labels(&trips), ["ICE 76", "ICE 2558"], "{name}");
            assert_eq!(trips[1].calls.len(), stops.len() + 1, "{name}: MOTIS train kept whole");
        }
    }
}

#[test]
fn motis_merges_into_the_train_sharing_most_stations() {
    let stations: Vec<Station> =
        ["A", "B", "C", "D"].iter().map(|&n| station(n, Some(n))).chain([station("E", None)]).collect();
    let visits = run("1", "IC 1", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:01"), (3, "11:00", "")])
        .into_iter()
        .chain(run("2", "IC 2", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:01"), (2, "10:00", "")]));
    let motis = motis_trip(
        "Train",
        Some('F'),
        &[(0, "", "08:00"), (1, "09:00", "09:01"), (2, "10:00", "10:01"), (4, "12:00", "")],
    );
    let trips = merge(&stations, &boards(visits), raw(vec![("m", motis)]));
    assert_eq!(labels(&trips), ["IC 1", "IC 2"]);
    assert_eq!(trips[0].calls.len(), 3);
    assert_eq!(calls_of(&trips[1]), [(0, true), (1, true), (2, true), (4, false)]);
}

#[test]
fn motis_merges_into_the_first_of_equally_matching_trains() {
    // IC 1 and IC 2 share A and B at the same times, so MOTIS matches both at
    // two stations; the first run by id takes it, however the candidates hash.
    let stations: Vec<Station> =
        ["A", "B", "C", "D"].iter().map(|&n| station(n, Some(n))).chain([station("E", None)]).collect();
    for _ in 0..32 {
        let visits = run("1", "IC 1", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:01"), (2, "10:00", "")])
            .into_iter()
            .chain(run("2", "IC 2", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:01"), (3, "11:00", "")]));
        let motis = motis_trip("Train", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:01"), (4, "12:00", "")]);
        let trips = merge(&stations, &boards(visits), raw(vec![("m", motis)]));
        assert_eq!(labels(&trips), ["IC 1", "IC 2"]);
        assert_eq!(calls_of(&trips[0]), [(0, true), (1, true), (2, true), (4, false)]);
        assert_eq!(calls_of(&trips[1]), [(0, true), (1, true), (3, true)]);
    }
}

#[test]
fn motis_stops_merged_into_a_db_train_only_where_db_lacks_them() {
    const MANNHEIM: usize = 0;
    const SUED: usize = 1;
    const FULDA: usize = 2;
    const FRANKFURT_HBF: usize = 3;
    const AIRPORT: usize = 4;
    const HANAU: usize = 5;
    const KASSEL: usize = 6;
    const GOETTINGEN: usize = 7;
    const HAMBURG: usize = 8;
    const COPENHAGEN: usize = 9;
    const BEFORE: usize = 10;
    let stations = vec![
        station("Mannheim Hbf", Some("Mannheim Hbf")),
        station("Frankfurt (Main) Südbahnhof", Some("Frankfurt(Main)Süd")),
        station("Fulda", Some("Fulda")),
        station("Frankfurt (Main) Hbf", Some("Frankfurt(Main)Hbf")),
        station("Frankfurt (Main) Flughafen Fernbahnhof", None),
        station("Hanau Hbf", Some("Hanau Hbf")),
        station("Kassel Bahnhof Wilhelmshöhe", Some("Kassel-Wilhelmshöhe")),
        station("Göttingen", Some("Göttingen")),
        station("Hamburg Hbf", Some("Hamburg Hbf")),
        station("København H", None),
        station("Somewhere", None),
    ];
    // DB's ICE 378: Mannheim 06:39, Frankfurt Süd 07:15, Fulda 08:21; its
    // paths name Hanau, Kassel and Göttingen, whose boards weren't fetched.
    let ice = || {
        let visits = run(
            "r",
            "ICE 378",
            Some('F'),
            &[(MANNHEIM, "", "06:39"), (SUED, "07:15", "07:17"), (FULDA, "08:21", "08:23")],
        );
        let visits = path_after(visits, SUED, &["Hanau Hbf", "Fulda", "Kassel-Wilhelmshöhe", "Göttingen"]);
        path_after(visits, FULDA, &["Kassel-Wilhelmshöhe", "Göttingen"])
    };
    let cases: [StopCase<'_>; 13] = [
        ("off DB's route (MOTIS via Frankfurt Hbf)", Some('F'), FRANKFURT_HBF, "07:20", &[], false),
        ("off DB's route, after its last stop", Some('F'), HAMBURG, "11:00", &[], false),
        ("unknown to DB inside its span (other name)", Some('F'), AIRPORT, "07:00", &[], false),
        ("unknown to DB after its span (abroad)", Some('F'), COPENHAGEN, "13:00", &[], true),
        ("unknown to DB before its span", Some('F'), BEFORE, "06:00", &[], true),
        // At the time of DB's first or last call it can only be that station, named differently.
        ("unknown to DB at its first call's time", Some('F'), BEFORE, "06:39", &[], false),
        ("unknown to DB at its last call's time", Some('F'), COPENHAGEN, "08:21", &[], false),
        ("on DB's route, no board", Some('F'), HANAU, "07:40", &[], true),
        ("on DB's route after its last board", Some('F'), KASSEL, "09:00", &[], true),
        (
            "board lists other long-distance trains that hour",
            Some('F'),
            GOETTINGEN,
            "09:30",
            &[(at("09:00"), 'F')],
            false,
        ),
        ("board covers only regional trains", Some('F'), GOETTINGEN, "09:30", &[(at("09:00"), 'N')], true),
        ("board covers another hour", Some('F'), GOETTINGEN, "09:30", &[(at("10:00"), 'F')], true),
        ("MOTIS train without class is never covered", None, GOETTINGEN, "09:30", &[(at("09:00"), 'F')], true),
    ];
    for (name, class, extra, t, coverage, kept) in cases {
        let mut b = boards(ice());
        if !coverage.is_empty() {
            cover(&mut b, extra, coverage);
        }
        // MOTIS has Frankfurt Süd 5 minutes early; DB's time wins.
        let motis = motis_trip("ICE 378", class, &[(MANNHEIM, "", "06:39"), (SUED, "07:10", "07:12"), (extra, t, "")]);
        let trips = merge(&stations, &b, raw(vec![("m", motis)]));
        assert_eq!(labels(&trips), ["ICE 378"], "{name}");
        let mut expected = vec![(MANNHEIM, true), (SUED, true), (FULDA, true)];
        if kept {
            expected.push((extra, false));
        }
        assert_eq!(calls_of(&trips[0]), expected, "{name}");
        assert_eq!(trips[0].calls[1].arrival, time("07:15"), "{name}");
    }
}

#[test]
fn without_route_knowledge_motis_stops_are_kept() {
    // A DB train whose stations DB names nothing for (no names, no paths).
    let stations = vec![station("X", None), station("Y", None), station("Z", None)];
    let visits = run("r", "EC 8", Some('F'), &[(0, "", "08:00"), (2, "10:00", "")]);
    let motis = motis_trip("EC 8", Some('F'), &[(0, "", "08:00"), (1, "09:00", "09:01"), (2, "10:00", "")]);
    let trips = merge(&stations, &boards(visits), raw(vec![("m", motis)]));
    assert_eq!(labels(&trips), ["EC 8"]);
    assert_eq!(calls_of(&trips[0]), [(0, true), (2, true), (1, false)]);
}

#[test]
fn unmerged_motis_trains_lose_calls_their_board_rules_out() {
    let stations: Vec<Station> = ["A", "B", "C"].iter().map(|&n| station(n, Some(n))).collect();
    let mut b = Boards::new();
    cover(&mut b, 0, &[(at("08:00"), 'N')]);
    cover(&mut b, 1, &[(at("08:00"), 'F')]);
    let re = motis_trip("RE 4711", Some('N'), &[(0, "", "08:00"), (1, "08:30", "08:31"), (2, "09:00", "")]);
    let bus = motis_trip("Bus 12", None, &[(0, "", "08:10"), (2, "08:50", "")]);
    let trips = merge(&stations, &b, raw(vec![("bus", bus), ("re", re)]));
    assert_eq!(labels(&trips), ["Bus 12", "RE 4711"]);
    assert_eq!(calls_of(&trips[0]), [(0, false), (2, false)]);
    assert_eq!(calls_of(&trips[1]), [(1, false), (2, false)]);
}

#[test]
fn motis_trains_with_the_same_times_from_two_feeds_become_one() {
    // EFA and Transitous both know the tram; DB has nothing.
    let stations = vec![station("Bad Dürkheim", None), station("Mannheim", None)];
    let efa = motis_trip("RNV 4", None, &[(0, "", "05:13"), (1, "06:10", "")]);
    let transitous = motis_trip("4", None, &[(0, "", "05:14"), (1, "06:10", "")]);
    let trips = merge(&stations, &Boards::new(), raw(vec![("a-efa", efa), ("b-transitous", transitous)]));
    assert_eq!(labels(&trips), ["RNV 4"]);
    assert_eq!(trips[0].calls.len(), 2);
}

#[test]
fn copies_of_a_train_from_two_feeds_become_one_with_efa_times() {
    // ICE 273 without DB data, EFA's times are right: Transitous has it 10
    // minutes later at Karlsruhe (and 22 at Mannheim), one at Freiburg. The
    // feeds also disagree on its class.
    let stations: Vec<Station> =
        ["Mannheim", "Karlsruhe", "Offenburg", "Freiburg", "Basel"].iter().map(|&n| station(n, Some(n))).collect();
    let efa = motis_trip("ICE 273", Some('N'), &[(1, "06:49", "06:51"), (3, "08:00", "")]);
    let transitous = |karlsruhe: (&'static str, &'static str), mannheim: &'static str| {
        motis_trip(
            "ICE 273",
            Some('F'),
            &[
                (0, "", mannheim),
                (1, karlsruhe.0, karlsruhe.1),
                (2, "07:29", "07:30"),
                (3, "08:01", "08:03"),
                (4, "08:45", ""),
            ],
        )
    };
    let cases = [
        // Stops are taken only where the copies agree around them: Basel after
        // Freiburg, but not Mannheim and Offenburg next to Karlsruhe.
        (
            "disagreeing at Karlsruhe",
            transitous(("06:59", "07:01"), "06:37"),
            vec![(1, "06:49"), (3, "08:00"), (4, "08:45")],
        ),
        (
            "agreeing everywhere",
            transitous(("06:49", "06:51"), "06:15"),
            vec![(0, "06:15"), (1, "06:49"), (2, "07:29"), (3, "08:00"), (4, "08:45")],
        ),
    ];
    for (what, transitous, expected) in cases {
        // Either key order: EFA's copy is the base.
        for (efa_id, transitous_id) in [("efa:z:1:20261025", "0-delfi"), ("efa:a:1:20261025", "zz-delfi")] {
            let trips =
                merge(&stations, &Boards::new(), raw(vec![(transitous_id, transitous.clone()), (efa_id, efa.clone())]));
            assert_eq!(trips.len(), 1, "{what}, {efa_id}");
            let mut times: Vec<_> = trips[0].calls.iter().map(|c| (c.station, c.planned())).collect();
            times.sort_unstable();
            let expected: Vec<_> = expected.iter().map(|&(s, t)| (s, Some(at(t)))).collect();
            assert_eq!(times, expected, "{what}, {efa_id}");
        }
    }
}

#[test]
fn feed_trains_sharing_a_number_but_no_time_stay_apart() {
    // An hour apart: two runs whose feeds give the line's number ("RE7" → 7).
    let stations: Vec<Station> = ["A", "B"].iter().map(|&n| station(n, Some(n))).collect();
    let early = motis_trip("RE7", Some('N'), &[(0, "", "08:00"), (1, "08:30", "")]);
    let late = motis_trip("RE7", Some('N'), &[(0, "", "09:00"), (1, "09:30", "")]);
    let trips = merge(&stations, &Boards::new(), raw(vec![("a", early), ("b", late)]));
    assert_eq!(trips.len(), 2);
    // Two minutes apart at one station is the same train, whatever the class.
    let a = motis_trip("ICE 5", Some('F'), &[(0, "", "08:00"), (1, "08:30", "")]);
    let b = motis_trip("5", Some('N'), &[(0, "", "08:02"), (1, "08:45", "")]);
    let trips = merge(&stations, &Boards::new(), raw(vec![("a", a), ("b", b)]));
    assert_eq!(labels(&trips), ["ICE 5"]);
    // Trams and buses: line numbers, so no merging by number.
    let a = motis_trip("4", None, &[(0, "", "08:00"), (1, "08:30", "")]);
    let b = motis_trip("4", None, &[(0, "", "08:01"), (1, "08:45", "")]);
    let trips = merge(&stations, &Boards::new(), raw(vec![("a", a), ("b", b)]));
    assert_eq!(trips.len(), 2);
}

// ---------------------------------------------------------------- DB boards

/// `fetch_boards` from Westend and Osthafen against a fake DB and MOTIS whose
/// geocoder answers `geocode`; DB trains run via `middle`. `evas` are
/// Westend's, `middle`'s and Osthafen's: each test uses its own, as DB
/// answers are cached across tests. Discovered names, warnings, stations,
/// boards and the paths asked for.
async fn discover_between_westend_and_osthafen(
    geocode: axum::http::StatusCode,
    middle: &'static str,
    evas: [&'static str; 3],
) -> (Vec<String>, Vec<String>, Stations, Boards, Vec<String>) {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Request, State};
    use axum::http::StatusCode;

    #[derive(Clone)]
    struct Fake {
        log: Arc<Mutex<Vec<String>>>,
        geocode: StatusCode,
        middle: &'static str,
        evas: [&'static str; 3],
    }
    // ICE 1 leaves Westend at 08:00 UTC via `middle` (not in the corridor)
    // for Osthafen, arriving 12:00; ICE 2 arrives at Osthafen at 09:00.
    async fn answer(State(fake): State<Fake>, request: Request) -> (StatusCode, String) {
        let path = request.uri().path().to_owned();
        fake.log.lock().unwrap().push(path.clone());
        let (middle, [west, eva, east]) = (fake.middle, fake.evas);
        let body = if path.starts_with(&format!("/plan/{west}/")) {
            format!(
                r#"<s id="1001-2610241000-1"><tl f="F" t="p" o="80" c="ICE" n="1"/><dp pt="2610241000" ppth="{middle}|Osthafen"/></s>"#
            )
        } else if path.starts_with(&format!("/plan/{east}/")) {
            format!(
                r#"<s id="2002-2610241030-4"><tl f="F" t="p" o="80" c="ICE" n="2"/><ar pt="2610241100" ppth="Westend"/></s>
<s id="1001-2610241000-3"><tl f="F" t="p" o="80" c="ICE" n="1"/><ar pt="2610241400" ppth="Westend|{middle}"/></s>"#
            )
        } else if path.starts_with("/station/") {
            format!(r#"<station p="" meta="" name="{middle}" eva="{eva}" ds100="X" db="true"/>"#)
        } else if path == "/api/v1/geocode" {
            return (fake.geocode, "[]".to_owned());
        } else {
            String::new()
        };
        (StatusCode::OK, format!("<timetable>{body}</timetable>"))
    }
    let log = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake { log: log.clone(), geocode, middle, evas };
    let base = crate::testkit::serve(axum::Router::new().fallback(answer).with_state(fake)).await;
    let motis = crate::motis::Motis::new(&base, "umstieg-test").unwrap();
    let credentials = crate::db::Credentials { client_id: "id".to_owned(), api_key: "key".to_owned() };
    let db = crate::db::Db::new(&base, credentials, "umstieg-test").unwrap();

    let mut stations = Stations::default();
    let mut db_station =
        |name, eva| stations.intern(Sighting { name, eva: Some(eva), db_name: Some(name), ..Sighting::default() });
    let (westend, osthafen) = (db_station("Westend", evas[0]), db_station("Osthafen", evas[2]));
    let corridor = HashMap::from([(westend, (at("08:00"), at("08:00"))), (osthafen, (at("12:00"), at("12:00")))]);
    let (mut discovered, mut warnings) = (Vec::new(), Vec::new());
    let boards = super::fetch_boards(&motis, &db, &mut stations, &corridor, &mut discovered, &mut warnings).await;
    let asked = log.lock().unwrap().clone();
    (discovered, warnings, stations, boards, asked)
}

#[tokio::test]
async fn stations_on_db_train_paths_get_boards_until_the_train_arrives() {
    // MOTIS knows no Mittelfeld: no position, but nothing to warn about.
    let (discovered, warnings, stations, boards, log) = discover_between_westend_and_osthafen(
        axum::http::StatusCode::OK,
        "Mittelfeld",
        ["8099701", "8099702", "8099703"],
    )
    .await;

    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(discovered, ["Mittelfeld"]);
    let mittelfeld = index_of(&stations, "Mittelfeld");
    assert!(boards.contains_key(&mittelfeld));
    // From 30 minutes before ICE 1 leaves Westend to 30 after it reaches Osthafen, in Berlin hours.
    let mut asked: Vec<String> =
        log.iter().filter_map(|p| p.strip_prefix("/plan/8099702/261024/").map(str::to_owned)).collect();
    asked.sort();
    assert_eq!(asked, ["09", "10", "11", "12", "13", "14"]);
}

#[tokio::test]
async fn a_failed_motis_stop_lookup_for_a_discovered_station_is_a_warning() {
    let (discovered, warnings, stations, boards, _) = discover_between_westend_and_osthafen(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "Mittelberg",
        ["8099711", "8099712", "8099713"],
    )
    .await;

    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].starts_with("MOTIS stop lookup for Mittelberg: GET "), "{warnings:?}");
    assert!(warnings[0].ends_with("500 Internal Server Error"), "{warnings:?}");
    // The station is still used, from DB alone.
    assert_eq!(discovered, ["Mittelberg"]);
    let mittelberg = index_of(&stations, "Mittelberg");
    assert!(boards.contains_key(&mittelberg));
    assert_eq!(stations.list[mittelberg].position, None);
}
