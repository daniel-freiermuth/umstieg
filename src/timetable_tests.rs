#![expect(clippy::arithmetic_side_effects, reason = "fixture times")]

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use proptest::prelude::*;

use super::{
    Boards, Call, Footpath, Sighting, Station, Stations, Time, Trip, angle_between, bearing, change_points, db_places,
    finish, footpaths, from_motis, hours, merge, meters, propagate_delays, swedish, tv_station, whole_seconds,
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

/// A station known to DB under `board_name` (`None`: only MOTIS knows it).
fn station(name: &str, board_name: Option<&str>) -> Station {
    Station {
        name: name.to_owned(),
        eva: None,
        tv: None,
        board_name: board_name.map(str::to_owned),
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

fn call(station: usize, from_board: bool, arrival: &str, departure: &str) -> Call {
    Call { station, arrival: time(arrival), departure: time(departure), platform: None, cancelled: false, from_board }
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
    trip.calls.iter().map(|c| (c.station, c.from_board)).collect()
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
            from_board: true,
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
        for (from_board, has_arrival, has_departure, gap, dwell, arrival_delay, departure_delay) in stops {
            t += minutes(gap);
            let arrival = has_arrival.then(|| Time { planned: t, actual: t + minutes(arrival_delay.unwrap_or(0)) });
            t += minutes(dwell);
            let departure =
                has_departure.then(|| Time { planned: t, actual: t + minutes(departure_delay.unwrap_or(0)) });
            calls.push(Call { station: 0, arrival, departure, platform: None, cancelled: false, from_board });
        }
        let before: Vec<(bool, Time)> = calls
            .iter()
            .flat_map(|c| [c.arrival, c.departure].into_iter().flatten().map(move |t| (c.from_board, t)))
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
                let (from_board, old) = before[k];
                k += 1;
                prop_assert_eq!(new.planned, old.planned);
                let live = from_board || old.actual != old.planned;
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
        board_name: Some("Frankfurt(M) Flughafen Fernbf"),
        ..Sighting::default()
    });
    // DB's name is now known too, even without an EVA number.
    let again = stations.intern(Sighting { name: "Frankfurt(M) Flughafen Fernbf", ..Sighting::default() });
    assert_eq!((db, again), (motis, motis));
    assert_eq!(stations.list.len(), 1);
    assert_eq!(stations.list[0].board_name.as_deref(), Some("Frankfurt(M) Flughafen Fernbf"));
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
        tv: None,
        board_name: Some("Fulda"),
        tz: Some("Europe/Berlin"),
        position: Some((50.554, 9.684)),
        motis_id: Some("de-DELFI_de:06631:1234_G"),
    });
    stations.intern(Sighting {
        name: "Fulda",
        eva: Some("1234567"),
        tv: None,
        board_name: Some("Fulda Bf"),
        tz: Some("Europe/Paris"),
        position: Some((1.0, 1.0)),
        motis_id: Some("other"),
    });
    let s = &stations.list[a];
    assert_eq!(stations.list.len(), 1);
    assert_eq!(
        (s.eva.as_deref(), s.board_name.as_deref(), s.tz.as_deref(), s.position, s.motis_id.as_deref()),
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
    assert!(ice.calls.iter().all(|c| !c.from_board));

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
    let trips = merge(&stations, &boards(visits), HashMap::new(), Vec::new());
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
        let trips = merge(&stations, &boards(visits), HashMap::new(), Vec::new());
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
        let trips = merge(&stations, &boards(visits), HashMap::new(), Vec::new());
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
    let trips = merge(&stations, &boards(visits), raw(vec![("m", motis)]), Vec::new());
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
    merge(&stations, &boards(visits), raw(vec![("m", motis)]), Vec::new())
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
        let trips = merge(&stations, &boards(visits()), raw(vec![("m", motis)]), Vec::new());
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
    let trips = merge(&stations, &boards(visits), raw(vec![("m", motis)]), Vec::new());
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
        let trips = merge(&stations, &boards(visits), raw(vec![("m", motis)]), Vec::new());
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
        let trips = merge(&stations, &b, raw(vec![("m", motis)]), Vec::new());
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
    let trips = merge(&stations, &boards(visits), raw(vec![("m", motis)]), Vec::new());
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
    let trips = merge(&stations, &b, raw(vec![("bus", bus), ("re", re)]), Vec::new());
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
    let trips = merge(&stations, &Boards::new(), raw(vec![("a-efa", efa), ("b-transitous", transitous)]), Vec::new());
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
            let trips = merge(
                &stations,
                &Boards::new(),
                raw(vec![(transitous_id, transitous.clone()), (efa_id, efa.clone())]),
                Vec::new(),
            );
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
    let trips = merge(&stations, &Boards::new(), raw(vec![("a", early), ("b", late)]), Vec::new());
    assert_eq!(trips.len(), 2);
    // Two minutes apart at one station is the same train, whatever the class.
    let a = motis_trip("ICE 5", Some('F'), &[(0, "", "08:00"), (1, "08:30", "")]);
    let b = motis_trip("5", Some('N'), &[(0, "", "08:02"), (1, "08:45", "")]);
    let trips = merge(&stations, &Boards::new(), raw(vec![("a", a), ("b", b)]), Vec::new());
    assert_eq!(labels(&trips), ["ICE 5"]);
    // Trams and buses: line numbers, so no merging by number.
    let a = motis_trip("4", None, &[(0, "", "08:00"), (1, "08:30", "")]);
    let b = motis_trip("4", None, &[(0, "", "08:01"), (1, "08:45", "")]);
    let trips = merge(&stations, &Boards::new(), raw(vec![("a", a), ("b", b)]), Vec::new());
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
        |name, eva| stations.intern(Sighting { name, eva: Some(eva), board_name: Some(name), ..Sighting::default() });
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

// ---------------------------------------------------------------- Trafikverket

/// Stand-in stations around Emmaboda (positions as on the map, roughly).
const GOTEBORG: (f64, f64) = (57.709, 11.974);
const KARLSHAMN: (f64, f64) = (56.177, 14.866);

fn tv_world() -> std::sync::Arc<crate::testkit::TvWorld> {
    use crate::testkit::{TvRun, TvWorld};
    let run = |ident, product, traffic, stops| TvRun { ident, product, traffic, stops };
    TvWorld::new(
        vec![
            ("G", "Göteborg C", "SE", GOTEBORG.0, GOTEBORG.1),
            ("Av", "Alvesta", "SE", 56.899, 14.556),
            ("Lse", "Lessebo", "SE", 56.751, 15.270),
            ("Em", "Emmaboda", "SE", 56.630, 15.535),
            ("Ny", "Nybro", "SE", 56.744, 15.906),
            ("Cr", "Karlskrona C", "SE", 56.166, 15.590),
            ("Kh", "Karlshamn", "SE", KARLSHAMN.0, KARLSHAMN.1),
            ("Dk.kh", "Köpenhamn H", "DK", 55.672, 12.565),
            ("Bln", "Berlin Hbf", "DE", 52.525, 13.369),
        ],
        vec![
            // At Göteborg's board: via Lessebo and Emmaboda on to Nybro.
            run(
                "319",
                "SJ Regional",
                "Tåg",
                vec![("G", "", "06:04"), ("Lse", "08:50", "08:51"), ("Em", "09:27+3", "09:30+3"), ("Ny", "09:44", "")],
            ),
            // At Karlshamn's board: Karlskrona, Emmaboda, Alvesta.
            run(
                "7100",
                "Krösatågen",
                "Tåg",
                vec![("Kh", "", "07:00"), ("Cr", "07:40", "07:42"), ("Em", "08:30", "08:31"), ("Av", "09:30", "")],
            ),
            // At Karlshamn's board, on to Copenhagen and Berlin.
            run(
                "1060",
                "Öresundståg",
                "Tåg",
                vec![("Cr", "", "10:30"), ("Kh", "11:10", "11:12"), ("Dk.kh", "13:00", "13:05"), ("Bln", "15:00", "")],
            ),
            // Only at Emmaboda's board.
            run("7200", "Krösatågen", "Tåg", vec![("Em", "", "09:40"), ("Cr", "10:15", "")]),
            run("8000", "Pendeltåg", "Pendeltåg", vec![("Em", "", "09:45"), ("Ny", "10:00", "")]),
        ],
    )
}

#[tokio::test]
async fn sweden_comes_from_trafikverket_runs_and_junction_boards() {
    let world = tv_world();
    let url = crate::testkit::serve_trafikverket(&world).await;
    let tv = crate::trafikverket::Trafikverket::new(&url, "key", "umstieg-test").unwrap();
    let mut stations = Stations::default();
    // MOTIS's names and positions, a few metres from Trafikverket's.
    let origin = stations.intern(Sighting {
        name: "Göteborg Centralstation",
        position: Some(north(GOTEBORG, 50.0)),
        ..Sighting::default()
    });
    let destination = stations.intern(Sighting {
        name: "Karlshamn Resecentrum",
        position: Some(north(KARLSHAMN, 450.0)),
        ..Sighting::default()
    });
    let corridor = HashMap::from([(origin, (at("06:00"), at("07:00"))), (destination, (at("06:30"), at("11:00")))]);
    let (mut discovered, mut warnings) = (Vec::new(), Vec::new());
    let found = swedish(&tv, &mut stations, &corridor, (origin, destination), &mut discovered, &mut warnings).await;

    assert!(warnings.is_empty(), "{warnings:?}");
    // Emmaboda, a junction; the runs' ends: Nybro, Alvesta, Karlskrona.
    assert_eq!(discovered, ["Alvesta", "Emmaboda", "Karlskrona C", "Nybro"]);
    let mut log = world.log();
    log[1..3].sort();
    log[4..8].sort();
    assert_eq!(log[..3], ["stations", "board G", "board Kh"]);
    assert_eq!(log[4..8], ["board Av", "board Cr", "board Em", "board Ny"]);
    // All runs of the first boards; then the new ones, but not Emmaboda's
    // and Nybro's commuter train.
    assert!(
        log[3].starts_with("runs ") && log[3].contains("319") && log[3].contains("7100") && log[3].contains("1060")
    );
    assert_eq!(log[8], "runs 7200");
    assert_eq!(log.len(), 9);

    let tv_of = |i: usize| stations.list[i].tv.clone();
    assert_eq!((tv_of(origin), tv_of(destination)), (Some("G".to_owned()), Some("Kh".to_owned())));
    let mut labels: Vec<&str> = found.trips.iter().map(|(t, _)| t.label.as_str()).collect();
    labels.sort_unstable();
    assert_eq!(labels, ["Krösatågen 7100", "Krösatågen 7200", "SJ Regional 319", "Öresundståg 1060"]);
    let trip = |label: &str| &found.trips.iter().find(|(t, _)| t.label == label).unwrap().0;
    let names = |t: &Trip| t.calls.iter().map(|c| stations.list[c.station].name.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(trip("Öresundståg 1060")),
        ["Karlskrona C", "Karlshamn Resecentrum", "Köpenhamn H"],
        "no German stop"
    );
    let sj = trip("SJ Regional 319");
    assert_eq!(names(sj), ["Göteborg Centralstation", "Lessebo", "Emmaboda", "Nybro"]);
    assert_eq!((sj.class, &sj.numbers, sj.headsign.as_deref()), (Some('F'), &vec![319], Some("Nybro")));
    assert!(sj.calls.iter().all(|c| c.from_board && !c.cancelled && c.platform.as_deref() == Some("1")));
    assert_eq!(sj.calls[2].arrival, Some(Time { planned: at("09:27"), actual: at("09:30") }));
    let route = &found.trips.iter().find(|(t, _)| t.label == "SJ Regional 319").unwrap().1;
    assert_eq!(route, &HashSet::from(["Göteborg C", "Lessebo", "Emmaboda", "Nybro"].map(str::to_owned)));
    // The boards' stations cover every class in the hours they list trains.
    let emmaboda = index_of(&stations, "Emmaboda");
    let mut covered: Vec<&str> = found.coverage.iter().map(|(i, _)| stations.list[*i].name.as_str()).collect();
    covered.sort_unstable();
    let boarded = ["Alvesta", "Emmaboda", "Göteborg Centralstation", "Karlshamn Resecentrum", "Karlskrona C", "Nybro"];
    assert_eq!(covered, boarded);
    let (_, at_emmaboda) = found.coverage.iter().find(|(i, _)| *i == emmaboda).unwrap();
    assert!(['F', 'N', 'S'].iter().all(|&c| at_emmaboda.covers(at("09:40"), c)));
    assert!(!at_emmaboda.covers(at("12:40"), 'N'), "an hour without trains");

    // Transitous's SJ Regional 319: on to Kalmar (unknown to Trafikverket's
    // list here) and, wrongly, Karlskrona C, which its route doesn't have.
    // Its "Emmaboda station" is Trafikverket's Emmaboda, 100 m off.
    let kalmar = stations.intern(Sighting { name: "Kalmar C", ..Sighting::default() });
    let karlskrona = index_of(&stations, "Karlskrona C");
    let emmaboda_motis = stations.intern(Sighting {
        name: "Emmaboda station",
        position: Some(north((56.630, 15.535), 100.0)),
        ..Sighting::default()
    });
    assert_eq!(emmaboda_motis, emmaboda);
    let motis = motis_trip(
        "SJ Regional 319",
        Some('F'),
        &[(origin, "", "06:04"), (emmaboda, "09:27", "09:30"), (karlskrona, "10:30", "10:31"), (kalmar, "10:20", "")],
    );
    let trips = merge(&stations.list, &Boards::new(), raw(vec![("m", motis)]), found.trips);
    let sj = trips.iter().find(|t| t.label == "SJ Regional 319").unwrap();
    let names: Vec<&str> = sj.calls.iter().map(|c| stations.list[c.station].name.as_str()).collect();
    assert_eq!(names, ["Göteborg Centralstation", "Lessebo", "Emmaboda", "Nybro", "Kalmar C"]);
}

#[tokio::test]
async fn trafikverket_is_not_asked_without_swedish_stations() {
    let world = tv_world();
    let url = crate::testkit::serve_trafikverket(&world).await;
    let tv = crate::trafikverket::Trafikverket::new(&url, "key", "umstieg-test").unwrap();
    let mut stations = Stations::default();
    let mannheim =
        stations.intern(Sighting { name: "Mannheim Hbf", position: Some((49.479, 8.469)), ..Sighting::default() });
    let corridor = HashMap::from([(mannheim, (at("06:00"), at("07:00")))]);
    let (mut discovered, mut warnings) = (Vec::new(), Vec::new());
    let found = swedish(&tv, &mut stations, &corridor, (mannheim, mannheim), &mut discovered, &mut warnings).await;
    assert!(found.trips.is_empty() && found.coverage.is_empty() && warnings.is_empty());
    assert_eq!(world.log(), ["stations"], "the cached list, nothing else");

    let empty =
        swedish(&tv, &mut stations, &HashMap::new(), (mannheim, mannheim), &mut discovered, &mut warnings).await;
    assert!(empty.trips.is_empty());
    assert_eq!(world.log(), ["stations"], "no corridor, no request");
}

#[tokio::test]
async fn trafikverket_failures_are_warnings() {
    let world = tv_world();
    let url = crate::testkit::serve_trafikverket(&world).await;
    let tv = crate::trafikverket::Trafikverket::new(&url, "wrong", "umstieg-test").unwrap();
    let mut stations = Stations::default();
    let g = stations.intern(Sighting { name: "Göteborg C", position: Some(GOTEBORG), ..Sighting::default() });
    let corridor = HashMap::from([(g, (at("06:00"), at("07:00")))]);
    let (mut discovered, mut warnings) = (Vec::new(), Vec::new());
    let found = swedish(&tv, &mut stations, &corridor, (g, g), &mut discovered, &mut warnings).await;
    assert!(found.trips.is_empty());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("Invalid authentication"), "{warnings:?}");
}

#[test]
fn trafikverket_stations_match_by_position_then_name_in_sweden_only() {
    let st = |sig: &str, name: &str, country: &str, position| crate::trafikverket::Station {
        signature: sig.to_owned(),
        name: name.to_owned(),
        country: country.to_owned(),
        position: Some(position),
    };
    let list = [
        st("G", "Göteborg C", "SE", GOTEBORG),
        st("Gsv", "Göteborg Sävenäs", "SE", north(GOTEBORG, 500.0)),
        st("Dk.kh", "Köpenhamn H", "DK", (55.672, 12.565)),
    ];
    let sig = |position, name| tv_station(&list, position, name).map(|s| s.signature.as_str());
    assert_eq!(sig(Some(north(GOTEBORG, 100.0)), "Göteborg Centralstation"), Some("G"), "the nearest");
    assert_eq!(sig(Some(north(GOTEBORG, 400.0)), "x"), Some("Gsv"));
    assert_eq!(sig(Some(north(GOTEBORG, 1200.0)), "x"), None, "600 m at most");
    assert_eq!(sig(Some(north(GOTEBORG, 1200.0)), "Göteborg C"), Some("G"), "else by name");
    assert_eq!(sig(None, "Göteborg C"), Some("G"));
    assert_eq!(sig(Some((55.672, 12.565)), "Köpenhamn H"), None, "Danish stations list only Swedish trains");
}

#[test]
fn stations_with_different_trafikverket_signatures_stay_apart() {
    let mut stations = Stations::default();
    let a = stations.intern(Sighting {
        name: "Stockholm C",
        tv: Some("Cst"),
        position: Some((59.330, 18.058)),
        ..Sighting::default()
    });
    let b = stations.intern(Sighting {
        name: "Stockholm City",
        tv: Some("Sci"),
        position: Some(north((59.330, 18.058), 200.0)),
        ..Sighting::default()
    });
    // MOTIS's stop near both, without a signature, is the nearer one; the signature finds it again.
    let c = stations.intern(Sighting {
        name: "Stockholm Centralstation",
        position: Some(north((59.330, 18.058), 20.0)),
        ..Sighting::default()
    });
    let d = stations.intern(Sighting { name: "Cst", tv: Some("Cst"), ..Sighting::default() });
    assert_eq!((a == b, c, d), (false, a, a));
    assert_eq!(stations.list[a].tv.as_deref(), Some("Cst"));
}

fn tv_run(ident: &str, class: char, stops: &[&str]) -> crate::trafikverket::Run {
    let moment = |minute: usize| {
        let planned = at("08:00") + TimeDelta::minutes(i64::try_from(minute).unwrap() * 10);
        Some(crate::trafikverket::Moment { planned, live: None, cancelled: false })
    };
    crate::trafikverket::Run {
        key: crate::trafikverket::RunKey { ident: ident.to_owned(), date: String::new() },
        product: None,
        class,
        stops: stops
            .iter()
            .enumerate()
            .map(|(k, sig)| crate::trafikverket::Stop {
                signature: (*sig).to_owned(),
                arrival: moment(k),
                departure: moment(k),
                track: None,
            })
            .collect(),
    }
}

type Spans = HashMap<String, (DateTime<Utc>, DateTime<Utc>)>;

/// Trafikverket stations named by signature, at (lat, lon).
fn tv_stations(list: &[(&str, (f64, f64))]) -> Vec<crate::trafikverket::Station> {
    list.iter()
        .map(|&(sig, position)| crate::trafikverket::Station {
            signature: sig.to_owned(),
            name: sig.to_owned(),
            country: "SE".to_owned(),
            position: Some(position),
        })
        .collect()
}

#[test]
fn junctions_are_stations_runs_reach_from_three_directions() {
    // A star around J: north N, east E, south S, west W; T a through station
    // on the north–south line; F far off the line between O and D. The runs'
    // ends are boarded already: only junctions are left to find.
    let stations = tv_stations(&[
        ("J", (56.5, 15.0)),
        ("N", (56.8, 15.0)),
        ("NN", (56.9, 15.02)),
        ("E", (56.5, 15.5)),
        ("S", (56.2, 15.0)),
        ("W", (56.5, 14.5)),
        ("T", (56.65, 15.0)),
        ("F", (56.5, 19.0)),
        ("FE", (56.5, 19.5)),
        ("FN", (56.8, 19.0)),
        ("FS", (56.2, 19.0)),
    ]);
    let by_sig = stations.iter().map(|s| (s.signature.as_str(), s)).collect();
    let line = Some(((57.5, 15.0), (55.5, 15.0)));
    let ends: Spans = ["N", "NN", "E", "S", "W", "FE", "FN", "FS"]
        .into_iter()
        .map(|sig| (sig.to_owned(), (at("08:00"), at("08:00"))))
        .collect();
    let found = |runs: &[crate::trafikverket::Run], boarded: &Spans| {
        let mut sigs: Vec<String> = change_points(runs, &by_sig, line, boarded).into_keys().collect();
        sigs.sort();
        sigs
    };
    // North–south through T, plus an express skipping T: two directions at T.
    let line_runs = [tv_run("1", 'N', &["N", "T", "J", "S"]), tv_run("2", 'F', &["NN", "J", "S"])];
    assert_eq!(found(&line_runs, &ends), Vec::<String>::new(), "J and T: north and south only");
    let star = [tv_run("1", 'N', &["N", "T", "J", "S"]), tv_run("3", 'N', &["W", "J", "E"])];
    assert_eq!(found(&star, &ends), ["J"]);
    let commuter = [tv_run("1", 'N', &["N", "T", "J", "S"]), tv_run("3", 'S', &["W", "J", "E"])];
    assert_eq!(found(&commuter, &ends), Vec::<String>::new(), "commuter trains don't count");
    let mut boarded = ends.clone();
    boarded.insert("J".to_owned(), (at("08:00"), at("08:00")));
    assert_eq!(found(&star, &boarded), Vec::<String>::new(), "J's board is fetched already");
    let far = [tv_run("4", 'N', &["FN", "F", "FS"]), tv_run("5", 'N', &["F", "FE"])];
    assert_eq!(found(&far, &ends), Vec::<String>::new(), "F is far off the line");

    // The span: from the first run there to two hours after the last.
    let spans = change_points(&star, &by_sig, line, &ends);
    assert_eq!(spans["J"], (at("08:10"), at("10:20")));
}

#[test]
fn termini_nearest_the_destination_come_after_the_junctions() {
    // From O in the north to D in the south; runs end at stations along the way.
    let stations = tv_stations(&[
        ("O", (57.5, 15.0)),
        ("T1", (57.3, 15.0)),
        ("T2", (57.1, 15.0)),
        ("T3", (56.9, 15.0)),
        ("T4", (56.7, 15.0)),
        ("T5", (56.5, 15.0)),
        ("T6", (56.3, 15.0)),
        ("T7", (56.1, 15.0)),
        ("Far", (56.5, 19.0)),
        ("J", (56.0, 15.0)),
        ("JW", (56.0, 14.5)),
        ("JE", (56.0, 15.5)),
        ("JS", (55.8, 15.0)),
    ]);
    let by_sig = stations.iter().map(|s| (s.signature.as_str(), s)).collect();
    let line = Some(((57.5, 15.0), (55.5, 15.0)));
    let boarded: Spans = HashMap::from([("O".to_owned(), (at("08:00"), at("08:00")))]);
    let mut runs: Vec<crate::trafikverket::Run> =
        ["T1", "T2", "T3", "T4", "T5", "T6", "T7", "Far"].iter().map(|t| tv_run(t, 'N', &["O", t])).collect();
    runs.push(tv_run("S", 'S', &["O", "JS"]));
    // J: three directions (west, east, north), and an end too; as a junction
    // it takes none of the termini's places.
    runs.push(tv_run("j1", 'N', &["JW", "J", "JE"]));
    runs.push(tv_run("j2", 'N', &["T7", "J"]));
    let mut found: Vec<String> = change_points(&runs, &by_sig, line, &boarded).into_keys().collect();
    found.sort();
    // J; then six termini, nearest D first: JE, JW, T7, T6, T5, T4. Not Far
    // (off the line), not JS (only a commuter train ends there), not T1–T3.
    assert_eq!(found, ["J", "JE", "JW", "T4", "T5", "T6", "T7"]);
}

#[test]
fn bearings_and_angles() {
    let o = (56.0, 15.0);
    let close = |a: f64, b: f64| (a - b).abs() < 0.5;
    assert!(close(bearing(o, (57.0, 15.0)), 0.0));
    assert!(close(bearing(o, (56.0, 16.0)), 89.6));
    assert!(close(bearing(o, (55.0, 15.0)), 180.0));
    assert!(close(bearing(o, (56.0, 14.0)), 270.4));
    assert!(close(bearing(o, (57.0, 16.0)), 28.5), "a degree of longitude is shorter up north");
    assert!(close(bearing(o, (55.0, 14.0)), 209.9));
    assert!(close(bearing(o, (57.0, 16.0)), 28.5), "a degree of longitude is shorter up north");
    assert!(close(bearing(o, (55.0, 14.0)), 209.9));
    assert_eq!(angle_between(10.0, 350.0), 20.0);
    assert_eq!(angle_between(350.0, 10.0), 20.0);
    assert_eq!(angle_between(90.0, 270.0), 180.0);
    assert_eq!(angle_between(45.0, 45.0), 0.0);
}

#[test]
fn transitous_copies_merge_into_trafikverket_runs_whatever_their_class() {
    // Trafikverket's Öresundståg 1036 (regional) Lund–Karlshamn; Transitous
    // calls it long-distance, adds a stop it doesn't know between (a stop
    // under another name) and one after its last Swedish stop.
    let stations = vec![
        station("Lund", Some("Lund C")),
        station("Hässleholm", Some("Hässleholm C")),
        station("Karlshamn", Some("Karlshamn")),
        station("Hassleholm (other feed)", None),
        station("Ronneby", None),
    ];
    let tv = Trip {
        label: "Öresundståg 1036".to_owned(),
        numbers: vec![1036],
        class: Some('N'),
        headsign: Some("Karlshamn".to_owned()),
        calls: vec![call(0, true, "", "09:41"), call(1, true, "10:14", "10:16"), call(2, true, "11:21", "")],
        transitous: false,
    };
    let route = HashSet::from(["Lund C", "Hässleholm C", "Karlshamn"].map(str::to_owned));
    let motis = motis_trip(
        "1036",
        Some('F'),
        &[(0, "", "09:41"), (3, "10:14", "10:16"), (2, "11:21", "11:22"), (4, "11:48", "")],
    );
    let trips = merge(&stations, &Boards::new(), raw(vec![("m", motis)]), vec![(tv, route)]);
    assert_eq!(labels(&trips), ["Öresundståg 1036"]);
    assert_eq!(calls_of(&trips[0]), [(0, true), (1, true), (2, true), (4, false)]);
    assert!(trips[0].transitous);
}

#[tokio::test]
async fn the_build_says_whether_trafikverket_trains_are_in_it() {
    let world = tv_world();
    let url = crate::testkit::serve_trafikverket(&world).await;
    let tv = crate::trafikverket::Trafikverket::new(&url, "key", "umstieg-test").unwrap();
    let motis = crate::motis::Motis::new("http://127.0.0.1:9", "umstieg-test").unwrap();
    let located = |name: &str, position: (f64, f64), arrival: &str, departure: &str| {
        let mut p = stop(name, arrival, departure);
        (p.lat, p.lon) = (Some(position.0), Some(position.1));
        p
    };
    let end = |name: &str, position: (f64, f64)| Stop {
        id: name.to_owned(),
        name: name.to_owned(),
        position: Some(position),
    };
    // Through Sweden: Göteborg → Karlshamn.
    let (g, kh) = (north(GOTEBORG, 50.0), north(KARLSHAMN, 50.0));
    let swedish = Itinerary {
        legs: vec![ride(
            "t",
            vec![located("Göteborg Centralstation", g, "", "06:55"), located("Karlshamn Resecentrum", kh, "11:21", "")],
        )],
    };
    let (from, to) = (end("Göteborg Centralstation", g), end("Karlshamn Resecentrum", kh));
    let built = super::build(&motis, None, Some(&tv), &[swedish], &from, &to, false).await;
    assert!(built.trafikverket, "{:?}", built.warnings);
    assert!(built.timetable.trips.iter().any(|t| t.label == "Öresundståg 1060"), "at Karlshamn at 11:10");

    // Elsewhere: nothing from Trafikverket, the MOTIS trains as they are.
    let (a, b) = ((49.479, 8.469), (49.404, 8.676));
    let german = Itinerary {
        legs: vec![ride("t", vec![located("Mannheim Hbf", a, "", "08:00"), located("Heidelberg Hbf", b, "08:15", "")])],
    };
    let built =
        super::build(&motis, None, Some(&tv), &[german], &end("Mannheim Hbf", a), &end("Heidelberg Hbf", b), false)
            .await;
    assert!(!built.trafikverket);
    assert_eq!(built.timetable.trips.len(), 1);
}
