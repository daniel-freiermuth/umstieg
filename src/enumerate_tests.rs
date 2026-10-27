#![expect(clippy::arithmetic_side_effects, reason = "fixture times")]

use std::collections::HashSet;

use chrono::{DateTime, TimeDelta, Utc};
use proptest::prelude::*;

use super::*;
use crate::testkit::{Builder, Spec, at, ride, show, spec, timetable};

const fn min(m: i64) -> TimeDelta {
    TimeDelta::minutes(m)
}

/// Generous limits departing `from`–`until`; tests narrow what they check.
fn limits(tt: &Timetable, from: &str, until: &str) -> Limits {
    Limits {
        origin: tt.origin,
        depart_from: at(from),
        depart_until: at(until),
        max_changes: 5,
        extra_time: TimeDelta::hours(12),
        min_transfer: min(5),
        missed_by: TimeDelta::zero(),
        keep_detours: false,
        forward_only: false,
        max_nodes: 1_000_000,
    }
}

fn station(tt: &Timetable, name: &str) -> usize {
    tt.stations.iter().position(|s| s.name == name).unwrap_or_else(|| panic!("no station {name}"))
}

fn walk(tt: &Timetable, from: &str, to: &str) -> Step {
    let (from, to) = (station(tt, from), station(tt, to));
    Step::Walk { footpath: tt.footpaths.iter().position(|f| f.from == from && f.to == to).unwrap() }
}

/// The connections found, sorted; each must be listed once.
fn shown(tt: &Timetable, result: &Enumeration) -> Vec<String> {
    let mut out: Vec<String> = result.chains.iter().map(|c| show(tt, &c.steps)).collect();
    out.sort();
    let unique: HashSet<&String> = out.iter().collect();
    assert_eq!(unique.len(), out.len(), "a connection is listed twice: {out:#?}");
    out
}

fn listed(tt: &Timetable, limits: &Limits) -> Vec<String> {
    shown(tt, &enumerate(&Network::new(tt).unwrap(), limits).unwrap())
}

fn sorted(expected: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = expected.iter().map(|&s| s.to_owned()).collect();
    v.sort();
    v
}

#[test]
fn departure_window_includes_both_ends() {
    let mut b = Builder::new();
    for (label, dep, arr) in
        [("RE 1", "07:59", "08:59"), ("RE 2", "08:00", "09:00"), ("RE 3", "09:00", "10:00"), ("RE 4", "09:01", "10:01")]
    {
        b.trip(label, &[("A", "", dep), ("D", arr, "")]);
    }
    let tt = b.build("A", "D");
    assert_eq!(listed(&tt, &limits(&tt, "08:00", "09:00")), sorted(&["RE 2 A-D", "RE 3 A-D"]));
}

#[test]
fn travel_time_is_at_most_fastest_in_window_plus_extra() {
    let mut b = Builder::new();
    // Faster, but leaving before the window: doesn't set the limit.
    b.trip("ICE 1", &[("A", "", "07:50"), ("D", "08:20", "")]);
    b.trip("RE 2", &[("A", "", "08:00"), ("D", "09:00", "")]); // 1h, the fastest
    b.trip("RE 3", &[("A", "", "08:30"), ("D", "09:45", "")]); // 1h15
    b.trip("RE 4", &[("A", "", "08:40"), ("D", "09:56", "")]); // 1h16
    // 1h15 from its own departure, ending an hour after the window.
    b.trip("RE 5", &[("A", "", "09:00"), ("B", "09:30", "")]);
    b.trip("RB 6", &[("B", "", "09:40"), ("D", "10:15", "")]);
    let tt = b.build("A", "D");
    let cases: [(i64, &[&str]); 3] = [
        (0, &["RE 2 A-D"]),
        (15, &["RE 2 A-D", "RE 3 A-D", "RE 5 A-B / RB 6 B-D"]),
        (16, &["RE 2 A-D", "RE 3 A-D", "RE 4 A-D", "RE 5 A-B / RB 6 B-D"]),
    ];
    for (extra, expected) in cases {
        let limits = Limits { extra_time: min(extra), ..limits(&tt, "08:00", "09:00") };
        let result = enumerate(&Network::new(&tt).unwrap(), &limits).unwrap();
        assert_eq!(result.fastest, Some(min(60)));
        assert_eq!(shown(&tt, &result), sorted(expected), "extra {extra} min");
    }
}

/// `fastest` is a connection the enumeration can list: it leaves inside the
/// window, takes a train first, and never walks twice in a row. Otherwise
/// the limit is cut below every real connection and nothing is listed.
#[test]
fn fastest_is_a_connection_the_search_can_list() {
    let mut wait_for_later_train = Builder::new();
    wait_for_later_train.trip("RE 1", &[("A", "", "08:00"), ("D", "10:00", "")]);
    wait_for_later_train.trip("ICE 2", &[("A", "", "08:30"), ("D", "09:00", "")]); // after the window
    let mut walk_first = Builder::new();
    walk_first.trip("RE 1", &[("A", "", "08:00"), ("D", "09:00", "")]);
    walk_first.walk("A", "A2", 1);
    walk_first.trip("RE 5", &[("A2", "", "08:01"), ("D", "08:31", "")]);
    let mut two_walks = Builder::new();
    two_walks.trip("RE 1", &[("A", "", "08:00"), ("B", "08:20", "")]);
    two_walks.walk("B", "C", 2);
    two_walks.walk("C", "C2", 2);
    two_walks.trip("RE 2", &[("C2", "", "08:30"), ("D", "08:40", "")]);
    two_walks.trip("RE 3", &[("B", "", "08:30"), ("D", "09:00", "")]);
    let cases =
        [(wait_for_later_train, 120, "RE 1 A-D"), (walk_first, 60, "RE 1 A-D"), (two_walks, 60, "RE 1 A-B / RE 3 B-D")];
    for (b, fastest, only) in cases {
        let tt = b.build("A", "D");
        let limits = Limits { extra_time: TimeDelta::zero(), ..limits(&tt, "08:00", "08:00") };
        let result = enumerate(&Network::new(&tt).unwrap(), &limits).unwrap();
        assert_eq!((result.fastest, shown(&tt, &result)), (Some(min(fastest)), vec![only.to_owned()]));
    }
}

/// Pruning by remaining travel time must not use planned ride times where the
/// train runs faster live: ICE 2 leaves 10 min late and makes up 6.
#[test]
fn a_train_catching_up_its_delay_is_not_pruned() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("C", "08:30", "")]);
    b.trip("ICE 2", &[("C", "", "08:21+10"), ("D", "09:21+4", "")]);
    let tt = b.build("A", "D");
    let limits = Limits { extra_time: TimeDelta::zero(), ..limits(&tt, "08:00", "08:00") };
    let result = enumerate(&Network::new(&tt).unwrap(), &limits).unwrap();
    assert_eq!((result.fastest, shown(&tt, &result)), (Some(min(85)), vec!["RE 1 A-C / ICE 2 C-D".to_owned()]));
}

/// Merged data can lack a departure time at a stop in the middle: the train
/// is ridden through it (and can be left there), but not boarded there.
#[test]
fn a_stop_without_departure_time_is_ridden_through() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "08:30", ""), ("D", "09:00", "")]);
    b.walk("B", "W", 1);
    let tt = b.build("A", "D");
    assert_eq!(listed(&tt, &limits(&tt, "08:00", "08:00")), ["RE 1 A-D"]);
    let net = Network::new(&tt).unwrap();
    let (a, bb) = (station(&tt, "A"), station(&tt, "B"));
    assert_eq!((net.lower_bound[a], net.bound[a]), (Some(min(60)), Some(min(60))));
    let fastest = fastest_journeys(&tt).unwrap();
    assert_eq!((fastest[a], fastest[bb]), (Some(min(60)), None));
}

fn three_ways() -> Timetable {
    let mut b = Builder::new();
    b.trip("RB 1", &[("A", "", "08:00"), ("B", "08:30", "")]);
    b.trip("RB 2", &[("B", "", "08:40"), ("C", "09:10", "")]);
    b.trip("RB 3", &[("C", "", "09:20"), ("D", "09:50", "")]);
    b.trip("IC 4", &[("A", "", "08:05"), ("C", "08:50", "")]);
    b.trip("ICE 5", &[("A", "", "08:10"), ("D", "09:40", "")]);
    b.build("A", "D")
}

#[test]
fn max_changes_bounds_the_rides_and_deepening_lists_each_connection_once() {
    let tt = three_ways();
    let cases: [(usize, &[&str]); 3] = [
        (0, &["ICE 5 A-D"]),
        (1, &["ICE 5 A-D", "IC 4 A-C / RB 3 C-D"]),
        (2, &["ICE 5 A-D", "IC 4 A-C / RB 3 C-D", "RB 1 A-B / RB 2 B-C / RB 3 C-D"]),
    ];
    for (max_changes, expected) in cases {
        let limits = Limits { max_changes, ..limits(&tt, "08:00", "08:10") };
        let result = enumerate(&Network::new(&tt).unwrap(), &limits).unwrap();
        assert_eq!(shown(&tt, &result), sorted(expected), "max {max_changes} changes");
        assert_eq!(result.complete_up_to, Some(max_changes));
    }
}

#[test]
fn hitting_the_effort_cap_keeps_fewer_changes_complete() {
    let tt = three_ways();
    let net = Network::new(&tt).unwrap();
    let direct_only = enumerate(&net, &Limits { max_changes: 0, ..limits(&tt, "08:00", "08:10") }).unwrap().nodes;
    // (cap, complete up to)
    let cases = [(direct_only - 1, None), (direct_only, Some(0)), (1_000_000, Some(2))];
    for (max_nodes, complete) in cases {
        let limits = Limits { max_changes: 2, max_nodes, ..limits(&tt, "08:00", "08:10") };
        let result = enumerate(&net, &limits).unwrap();
        assert_eq!(result.complete_up_to, complete, "cap {max_nodes}");
        if complete == Some(0) {
            let direct: Vec<String> =
                result.chains.iter().filter(|c| c.changes() == 0).map(|c| show(&tt, &c.steps)).collect();
            assert_eq!(direct, ["ICE 5 A-D"]);
        }
    }
}

#[test]
fn no_connections_means_no_fastest_and_nothing_cut() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("D", "", "08:00"), ("A", "09:00", "")]);
    let tt = b.build("A", "D");
    let result = enumerate(&Network::new(&tt).unwrap(), &limits(&tt, "08:00", "09:00")).unwrap();
    assert!(result.chains.is_empty());
    assert_eq!((result.fastest, result.complete_up_to), (None, Some(5)));
}

#[test]
fn changes_missed_by_up_to_missed_by_are_listed_with_negative_buffer() {
    let mut b = Builder::new();
    b.trip("ICE 1", &[("A", "", "08:00"), ("B", "08:57+3", "")]); // arrives 09:00
    b.trip("RE 54", &[("B", "", "08:54"), ("D", "09:54", "")]);
    b.trip("RE 55", &[("B", "", "08:55"), ("D", "09:55", "")]);
    b.trip("RE 56", &[("B", "", "08:50+6"), ("D", "09:50+6", "")]); // leaves 08:56
    b.trip("RE 60", &[("B", "", "09:00"), ("D", "10:00", "")]);
    b.trip("RE 61", &[("B", "", "09:01"), ("D", "10:01", "")]);
    let tt = b.build("A", "D");
    let cases: [(i64, &[(&str, i64)]); 3] = [
        // Any change that works is listed, however short.
        (0, &[("RE 60", 0), ("RE 61", 1)]),
        (5, &[("RE 55", -5), ("RE 56", -4), ("RE 60", 0), ("RE 61", 1)]),
        (6, &[("RE 54", -6), ("RE 55", -5), ("RE 56", -4), ("RE 60", 0), ("RE 61", 1)]),
    ];
    for (missed_by, expected) in cases {
        let limits = Limits { missed_by: min(missed_by), ..limits(&tt, "08:00", "08:00") };
        let mut found: Vec<(String, i64)> = enumerate(&Network::new(&tt).unwrap(), &limits)
            .unwrap()
            .chains
            .iter()
            .map(|c| {
                let second = c.rides().nth(1).unwrap().0;
                (tt.trips[second].label.clone(), c.min_slack(&tt).unwrap().unwrap().num_minutes())
            })
            .collect();
        found.sort();
        let expected: Vec<(String, i64)> = expected.iter().map(|&(l, s)| (l.to_owned(), s)).collect();
        assert_eq!(found, expected, "missed by {missed_by}");
    }
}

#[test]
fn no_station_is_visited_twice_not_even_passing_through() {
    let mut b = Builder::new();
    b.trip("RB 1", &[("A", "", "08:00"), ("B", "08:30", "08:31"), ("C", "09:00", "")]);
    // From C back through B (passed on RB 1): only boardable at B.
    b.trip("RB 2", &[("C", "", "09:10"), ("B", "09:40", "09:41"), ("D", "10:10", "")]);
    // Through the origin.
    b.trip("RB 3", &[("B", "", "08:40"), ("A", "09:00", "09:01"), ("D", "09:30", "")]);
    let tt = b.build("A", "D");
    assert_eq!(listed(&tt, &limits(&tt, "08:00", "08:00")), ["RB 1 A-B / RB 2 B-D"]);
}

#[test]
fn walks_between_and_after_trains_but_never_two_in_a_row() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "08:30", "")]);
    b.walk("B", "B2", 5);
    b.trip("RE 2", &[("B2", "", "08:40"), ("D", "09:10", "")]);
    b.walk("B", "B3", 3);
    b.walk("B3", "B4", 2);
    b.trip("RE 3", &[("B4", "", "08:45"), ("D", "09:05", "")]);
    b.trip("RE 4", &[("A", "", "08:02"), ("C", "08:20", "")]);
    b.walk("C", "D", 15);
    let tt = b.build("A", "D");
    assert_eq!(
        listed(&tt, &limits(&tt, "08:00", "08:10")),
        sorted(&["RE 1 A-B / walk B-B2 / RE 2 B2-D", "RE 4 A-C / walk C-D"])
    );
}

#[test]
fn walk_into_the_destination_counts_towards_travel_time() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("D", "09:00", "")]);
    b.trip("RE 2", &[("A", "", "08:00"), ("C", "08:40", "")]);
    b.walk("C", "D", 21);
    let tt = b.build("A", "D");
    for (extra, expected) in [(0, &["RE 1 A-D"][..]), (1, &["RE 1 A-D", "RE 2 A-C / walk C-D"])] {
        let limits = Limits { extra_time: min(extra), ..limits(&tt, "08:00", "08:00") };
        assert_eq!(listed(&tt, &limits), sorted(expected), "extra {extra}");
    }
}

#[test]
fn extreme_limits_saturate_instead_of_overflowing() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("D", "09:00", "")]);
    b.trip("RE 2", &[("A", "", "08:00"), ("C", "08:40", "")]);
    b.walk("C", "D", 21);
    let tt = b.build("A", "D");
    let limits = Limits {
        depart_until: DateTime::<Utc>::MAX_UTC,
        max_changes: usize::MAX,
        extra_time: TimeDelta::MAX,
        missed_by: TimeDelta::MAX,
        max_nodes: 1000,
        ..limits(&tt, "08:00", "08:00")
    };
    let result = enumerate(&Network::new(&tt).unwrap(), &limits).unwrap();
    assert_eq!(shown(&tt, &result), sorted(&["RE 1 A-D", "RE 2 A-C / walk C-D"]));
    assert_eq!(result.fastest, Some(min(60)));
}

#[test]
fn a_walk_too_long_to_end_is_never_taken() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("D", "09:00", "")]);
    b.trip("RE 2", &[("A", "", "08:00"), ("C", "08:40", "")]);
    b.walk("C", "D", 21);
    let mut tt = b.build("A", "D");
    for fp in &mut tt.footpaths {
        fp.duration = TimeDelta::MAX;
    }
    assert_eq!(listed(&tt, &limits(&tt, "08:00", "08:00")), ["RE 1 A-D"]);
    // Walking all the way: as long as the walk.
    assert_eq!(fastest_journeys(&tt).unwrap()[station(&tt, "C")], Some(TimeDelta::MAX));
    let chain = Chain { steps: vec![ride(&tt, "RE 2", "A", "C"), walk(&tt, "C", "D")] };
    let err = chain.arrival(&tt).unwrap_err().to_string();
    assert!(err.contains("arrival after the last walk") && err.contains("out of range"), "{err}");
}

#[test]
fn a_timetable_referring_to_a_missing_station_is_an_error() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("D", "09:00", "")]);
    let mut tt = b.build("A", "D");
    tt.trips[0].calls[0].station = 99;
    assert_eq!(Network::new(&tt).err().map(|e| e.to_string()), Some("no station 99 in the timetable".to_owned()));
    let chain = Chain { steps: vec![Step::Ride { trip: 7, board: 0, alight: 1 }] };
    assert!(chain.departure(&tt).is_err() && chain.arrival(&tt).is_err() && chain.transfers(&tt).is_err());
}

#[test]
fn cancelled_stops_are_ridden_through_but_not_boarded_or_left() {
    let mut b = Builder::new();
    let ice =
        b.trip("ICE 1", &[("A", "", "08:00"), ("B", "08:30", "08:31"), ("C", "09:00", "09:01"), ("D", "09:30", "")]);
    b.cancel(ice, "B");
    b.trip("RE 2", &[("B", "", "08:45"), ("D", "09:20", "")]);
    let re3 = b.trip("RE 3", &[("A", "", "08:01"), ("D", "09:00", "")]);
    b.cancel(re3, "A");
    let re7 = b.trip("RE 7", &[("A", "", "08:03"), ("D", "08:50", "")]);
    b.cancel(re7, "D");
    let tt = b.build("A", "D");
    assert_eq!(listed(&tt, &limits(&tt, "08:00", "08:10")), ["ICE 1 A-D"]);
}

/// S-Bahn Mannheim → Heidelberg for an ECE that skips Mannheim: riding away
/// from the goal (Heidelberg is farther than Mannheim by the best case) pays
/// off here, and `forward_only` drops it.
#[test]
fn forward_only_drops_rides_that_do_not_reduce_the_best_case() {
    let mut b = Builder::new();
    b.trip("ICE 1", &[("Mannheim", "", "09:00"), ("Frankfurt", "09:30", "")]);
    b.trip("S 2", &[("Mannheim", "", "08:00"), ("Heidelberg", "08:15", "")]);
    b.trip("ECE 3", &[("Heidelberg", "", "08:30"), ("Frankfurt", "09:20", "")]);
    let tt = b.build("Mannheim", "Frankfurt");
    let all = ["ICE 1 Mannheim-Frankfurt", "S 2 Mannheim-Heidelberg / ECE 3 Heidelberg-Frankfurt"];
    for (forward_only, expected) in [(false, &all[..]), (true, &all[..1])] {
        let limits = Limits { forward_only, ..limits(&tt, "08:00", "09:00") };
        assert_eq!(listed(&tt, &limits), sorted(expected), "forward only {forward_only}");
    }
}

/// ICE 774 to Kassel, then ICE 772, which also left Mannheim: pointless, as
/// 772 could have been taken from the start. ICE 770 left Mannheim before the
/// chain did, so changing to it is fine.
#[test]
fn trains_leaving_the_origin_after_the_chain_are_not_boarded_later() {
    type Stop = (&'static str, &'static str);
    let mut b = Builder::new();
    let stops = |dep, fra: Stop, kassel: Stop, end: (&'static str, &'static str, &'static str)| {
        [("Mannheim", "", dep), ("Frankfurt", fra.0, fra.1), ("Kassel", kassel.0, kassel.1), end]
    };
    b.trip("ICE 774", &stops("08:00", ("08:40", "08:45"), ("10:00", "10:02"), ("Berlin", "12:00", "")));
    b.trip("ICE 772", &stops("08:30", ("09:10", "09:15"), ("10:30", "10:32"), ("Hamburg", "12:00", "")));
    b.trip("ICE 770", &stops("07:50", ("08:30", "08:35"), ("10:20", "10:22"), ("Hamburg", "12:10", "")));
    let tt = b.build("Mannheim", "Hamburg");
    let kept = ["ICE 772 Mannheim-Hamburg", "ICE 774 Mannheim-Kassel / ICE 770 Kassel-Hamburg"];
    let detours =
        ["ICE 774 Mannheim-Frankfurt / ICE 772 Frankfurt-Hamburg", "ICE 774 Mannheim-Kassel / ICE 772 Kassel-Hamburg"];
    let limits = |keep_detours| Limits { keep_detours, ..limits(&tt, "08:00", "08:30") };
    assert_eq!(listed(&tt, &limits(false)), sorted(&kept));
    assert_eq!(listed(&tt, &limits(true)), sorted(&[kept, detours].concat()));
}

/// Hamburg Hbf: ICE 2 arrives `arrive`, RJ 384 leaves 12:50 for Altona
/// (13:15/13:20) and København; S 1 leaves `s1` and reaches Altona 10 min later.
fn hamburg(arrive: &str, s1: (&str, &str)) -> Timetable {
    let mut b = Builder::new();
    b.trip("ICE 2", &[("Frankfurt", "", "08:00"), ("Hamburg", arrive, "")]);
    b.trip("RJ 384", &[("Hamburg", "", "12:50"), ("Altona", "13:15", "13:20"), ("København", "17:40", "")]);
    b.trip("S 1", &[("Hamburg", "", s1.0), ("Altona", s1.1, "")]);
    b.build("Frankfurt", "København")
}

/// Changing to a train that called at an earlier stop of the chain is a
/// detour if it was catchable there comfortably (≥ min transfer) and with at
/// least the worst buffer the detour leaves.
#[test]
fn detours_are_pruned_unless_they_raise_the_worst_buffer() {
    let direct = "ICE 2 Frankfurt-Hamburg / RJ 384 Hamburg-København";
    let detour = "ICE 2 Frankfurt-Hamburg / S 1 Hamburg-Altona / RJ 384 Altona-København";
    // (ICE arrival, S 1, min transfer, detour kept)
    let cases = [
        // 14 min direct vs 16 and 18 on the detour: kept.
        ("12:36", ("12:52", "13:02"), 5, true),
        // 14 min direct vs 14 and 20: no better, pruned.
        ("12:36", ("12:50", "13:00"), 5, false),
        // 3 min direct is no comfortable change, even against a 1 min one.
        ("12:47", ("12:48", "12:58"), 5, true),
        // … unless the minimum transfer is 3 min.
        ("12:47", ("12:48", "12:58"), 3, false),
    ];
    for (arrive, s1, min_transfer, kept) in cases {
        let tt = hamburg(arrive, s1);
        let limits = Limits { min_transfer: min(min_transfer), ..limits(&tt, "08:00", "08:00") };
        let expected = if kept { sorted(&[direct, detour]) } else { vec![direct.to_owned()] };
        assert_eq!(listed(&tt, &limits), expected, "{arrive} {s1:?} min transfer {min_transfer}");
        let keep = Limits { keep_detours: true, ..limits };
        assert_eq!(listed(&tt, &keep), sorted(&[direct, detour]), "keep detours");
    }
}

/// Arriving at Hamburg Hbf 11:51 and taking the RE 7 to Schleswig (11 min
/// change) for the RJ that leaves Hamburg Hbf at 12:38 is pruned.
#[test]
fn detour_to_a_later_stop_of_the_same_train_is_pruned() {
    let mut b = Builder::new();
    b.trip("ICE 1", &[("Frankfurt", "", "07:00"), ("Hamburg", "11:51", "")]);
    b.trip("RE 7", &[("Hamburg", "", "12:02"), ("Schleswig", "12:50", "")]);
    b.trip("RJ 384", &[("Hamburg", "", "12:38"), ("Schleswig", "13:20", "13:22"), ("København", "16:00", "")]);
    let tt = b.build("Frankfurt", "København");
    let direct = "ICE 1 Frankfurt-Hamburg / RJ 384 Hamburg-København";
    let detour = "ICE 1 Frankfurt-Hamburg / RE 7 Hamburg-Schleswig / RJ 384 Schleswig-København";
    assert_eq!(listed(&tt, &limits(&tt, "07:00", "07:00")), [direct]);
    let keep = Limits { keep_detours: true, ..limits(&tt, "07:00", "07:00") };
    assert_eq!(listed(&tt, &keep), sorted(&[direct, detour]));
}

/// Arriving at Hamburg 12:00, the RJ leaves 12:10 (10 min). The detour via
/// Harburg and Lüneburg changes 20 min at Hamburg and 20 at Lüneburg; its
/// worst buffer is the change at Harburg in between.
#[test]
fn detour_worst_buffer_counts_every_change_since_the_earlier_stop() {
    let direct = "ICE 1 Frankfurt-Hamburg / RJ 384 Hamburg-København";
    let detour = "ICE 1 Frankfurt-Hamburg / S 3 Hamburg-Harburg / RB 31 Harburg-Lüneburg / RJ 384 Lüneburg-København";
    // (RB 31 leaves Harburg, detour kept)
    let cases = [
        // 2 min at Harburg: the 10 min direct change is better, pruned.
        ("12:32", false),
        // 15 min at Harburg: every change of the detour beats 10 min, kept.
        ("12:45", true),
    ];
    for (rb, kept) in cases {
        let mut b = Builder::new();
        b.trip("ICE 1", &[("Frankfurt", "", "08:00"), ("Hamburg", "12:00", "")]);
        b.trip("S 3", &[("Hamburg", "", "12:20"), ("Harburg", "12:30", "")]);
        b.trip("RB 31", &[("Harburg", "", rb), ("Lüneburg", "12:55", "")]);
        b.trip("RJ 384", &[("Hamburg", "", "12:10"), ("Lüneburg", "13:15", "13:15"), ("København", "17:40", "")]);
        let tt = b.build("Frankfurt", "København");
        let limits = limits(&tt, "08:00", "08:00");
        let expected = if kept { sorted(&[direct, detour]) } else { vec![direct.to_owned()] };
        assert_eq!(listed(&tt, &limits), expected, "RB 31 at {rb}");
        let keep = Limits { keep_detours: true, ..limits };
        assert_eq!(listed(&tt, &keep), sorted(&[direct, detour]), "keep detours");
    }
}

/// RNV 4 → S 1 changing at Ludwigshafen Hbf or at Ludwigshafen Mitte: both listed.
#[test]
fn same_trains_changing_at_different_stations_are_all_kept() {
    let mut b = Builder::new();
    b.trip(
        "RNV 4",
        &[
            ("Bad Dürkheim", "", "08:00"),
            ("Lu Hbf", "08:20", "08:21"),
            ("Lu Mitte", "08:24", "08:25"),
            ("Mannheim", "08:30", ""),
        ],
    );
    b.trip("S 1", &[("Lu Hbf", "", "08:30"), ("Lu Mitte", "08:33", "08:34"), ("Heidelberg", "09:00", "")]);
    let tt = b.build("Bad Dürkheim", "Heidelberg");
    assert_eq!(
        listed(&tt, &limits(&tt, "08:00", "08:00")),
        sorted(&[
            "RNV 4 Bad Dürkheim-Lu Hbf / S 1 Lu Hbf-Heidelberg",
            "RNV 4 Bad Dürkheim-Lu Mitte / S 1 Lu Mitte-Heidelberg"
        ])
    );
}

#[test]
fn groups_by_first_and_last_train_feasible_first_then_arrival_changes_buffer() {
    let mut b = Builder::new();
    b.trip("ICE 1", &[("A", "", "08:00"), ("B", "08:30", "08:31"), ("C", "08:36+20", "")]);
    b.trip("RE 3", &[("B", "", "08:40"), ("C", "08:50", "08:51"), ("E", "09:00", "09:01"), ("D", "09:30", "")]);
    b.trip("S 6", &[("B", "", "08:32"), ("E", "08:55", "")]);
    b.walk("B", "E", 10);
    b.trip("ICE 2", &[("A", "", "08:10"), ("F", "08:40", "")]);
    b.trip("RE 4", &[("F", "", "08:45"), ("D", "09:20", "")]);
    b.trip("RE 5", &[("F", "", "08:40"), ("D", "09:25", "")]); // no buffer, but caught
    b.trip("RE 8", &[("F", "", "08:39"), ("D", "09:10", "")]);
    b.trip("ICE 9", &[("A", "", "08:20"), ("D", "09:20", "")]);
    b.trip("IC 10", &[("A", "", "08:25"), ("D", "09:20", "")]);
    let tt = b.build("A", "D");
    let r = |label, from, to| ride(&tt, label, from, to);
    let chains = [
        vec![r("ICE 2", "A", "F"), r("RE 8", "F", "D")], // missed by 1, arrives 09:10
        vec![r("ICE 1", "A", "C"), r("RE 3", "C", "D")], // missed by 5
        vec![r("ICE 1", "A", "B"), r("S 6", "B", "E"), r("RE 3", "E", "D")], // 2 changes, min 2
        vec![r("ICE 1", "A", "B"), r("RE 3", "B", "D")], // 10 min
        vec![r("ICE 1", "A", "B"), walk(&tt, "B", "E"), r("RE 3", "E", "D")], // 21 min
        vec![r("ICE 2", "A", "F"), r("RE 4", "F", "D")], // 09:20, one change
        vec![r("ICE 2", "A", "F"), r("RE 5", "F", "D")], // 09:25, zero buffer
        vec![r("IC 10", "A", "D")],                      // 09:20 direct, leaves 08:25
        vec![r("ICE 9", "A", "D")],                      // 09:20 direct, leaves 08:20
    ];
    let groups = group(&tt, chains.into_iter().map(|steps| Chain { steps }).collect()).unwrap();
    let got: Vec<Vec<String>> = groups.iter().map(|g| g.chains.iter().map(|c| show(&tt, &c.steps)).collect()).collect();
    let expected = [
        &["ICE 9 A-D"][..],
        &["IC 10 A-D"],
        &["ICE 2 A-F / RE 4 F-D"],
        &["ICE 2 A-F / RE 5 F-D"],
        &[
            "ICE 1 A-B / walk B-E / RE 3 E-D",
            "ICE 1 A-B / RE 3 B-D",
            "ICE 1 A-B / S 6 B-E / RE 3 E-D",
            "ICE 1 A-C / RE 3 C-D",
        ],
        &["ICE 2 A-F / RE 8 F-D"],
    ];
    assert_eq!(got, expected);
    for g in &groups {
        let trips: Vec<usize> = g.chains[0].rides().map(|(trip, _, _)| trip).collect();
        assert_eq!((g.first, g.last), (trips[0], trips[trips.len() - 1]));
    }
}

#[test]
fn chain_times_transfers_and_buffers() {
    let mut b = Builder::new();
    b.trip("ICE 1", &[("A", "", "08:00+2"), ("B", "08:30+4", "")]);
    b.walk("B", "B2", 5);
    b.trip("RE 2", &[("B2", "", "08:40"), ("C", "09:00", "09:02")]);
    b.trip("RE 3", &[("C", "", "09:05"), ("D", "09:30+1", "")]);
    b.walk("D", "Z", 7);
    let tt = b.build("A", "Z");
    let chain = Chain {
        steps: vec![
            ride(&tt, "ICE 1", "A", "B"),
            walk(&tt, "B", "B2"),
            ride(&tt, "RE 2", "B2", "C"),
            ride(&tt, "RE 3", "C", "D"),
            walk(&tt, "D", "Z"),
        ],
    };
    assert_eq!((chain.departure(&tt).unwrap(), chain.arrival(&tt).unwrap()), (at("08:02"), at("09:38")));
    assert_eq!(chain.changes(), 2);
    let transfers: Vec<_> = chain
        .transfers(&tt)
        .unwrap()
        .iter()
        .map(|t| (tt.stations[t.station].name.as_str(), t.departure, t.walk, t.slack))
        .collect();
    // At B: arrives 08:34, walks 5 min, RE 2 leaves 08:40.
    assert_eq!(transfers, [("B", at("08:40"), min(5), min(1)), ("C", at("09:05"), min(0), min(5))]);
    assert_eq!(chain.min_slack(&tt).unwrap(), Some(min(1)));

    let direct = Chain { steps: vec![ride(&tt, "RE 3", "C", "D")] };
    assert!(direct.transfers(&tt).unwrap().is_empty());
    assert_eq!((direct.min_slack(&tt).unwrap(), direct.arrival(&tt).unwrap()), (None, at("09:31")));
}

#[test]
fn earliest_arrival_by_train_from_a_start_time() {
    let mut b = Builder::new();
    // Listed before the train that reaches B: needs another pass.
    b.trip("RB 2", &[("B", "", "09:00"), ("C", "09:30", "")]);
    b.trip("RB 1", &[("A", "", "08:00"), ("B", "08:30+5", "")]);
    b.trip("RB 0", &[("A", "", "07:59"), ("G", "08:10", "")]);
    let rb4 = b.trip("RB 4", &[("A", "", "08:05"), ("X", "08:20", "08:21"), ("Y", "08:40", "")]);
    b.cancel(rb4, "X");
    let rb6 = b.trip("RB 6", &[("A", "", "08:10"), ("H", "08:30", "")]);
    b.cancel(rb6, "A");
    b.trip("RB 7", &[("A", "", "08:50"), ("K", "09:00", "")]); // after the latest departure
    b.trip("RB 8", &[("B", "", "08:31"), ("M", "08:50", "")]); // gone before RB 1 arrives
    b.trip("RB 9", &[("B", "", "08:35"), ("N", "08:50", "")]); // leaves as RB 1 arrives
    b.walk("C", "E", 5);
    b.walk("E", "F", 5); // a second walk
    b.walk("A", "W", 1); // a walk before any train
    let tt = b.build("A", "F");
    let ea = earliest_arrival(&tt, station(&tt, "A"), at("08:00"), at("08:45"), TimeDelta::zero()).unwrap();
    let cases = [
        ("A", Some("08:00")),
        ("B", Some("08:35")),
        ("C", Some("09:30")),
        ("E", Some("09:35")),
        ("F", None),
        ("G", None),
        ("X", None),
        ("Y", Some("08:40")),
        ("H", None),
        ("K", None),
        ("M", None),
        ("N", Some("08:50")),
        ("W", None),
    ];
    for (name, expected) in cases {
        assert_eq!(ea[station(&tt, name)], expected.map(at), "{name}");
    }
    // RB 8 is missed by 4 min.
    let ea = earliest_arrival(&tt, station(&tt, "A"), at("08:00"), at("08:45"), min(4)).unwrap();
    assert_eq!(ea[station(&tt, "M")], Some(at("08:50")));
}

/// A pass can improve trains but no walks; only a pass improving nothing
/// at all ends the scan. RB 3 is listed before RB 2, which reaches its start.
#[test]
fn earliest_arrival_scans_until_nothing_improves() {
    let mut b = Builder::new();
    b.trip("RB 3", &[("D", "", "09:00"), ("E", "09:30", "")]);
    b.trip("RB 2", &[("C", "", "08:40"), ("D", "08:50", "")]);
    b.trip("RB 1", &[("A", "", "08:00"), ("B", "08:30", "")]);
    b.walk("B", "C", 5);
    let tt = b.build("A", "E");
    let ea = earliest_arrival(&tt, station(&tt, "A"), at("08:00"), at("08:00"), TimeDelta::zero()).unwrap();
    assert_eq!(ea[station(&tt, "E")], Some(at("09:30")));
}

/// Missed connections are listed even when nothing else gets there.
#[test]
fn missed_connections_are_listed_when_they_are_all_there_is() {
    let mut b = Builder::new();
    b.trip("ICE 1", &[("A", "", "08:00"), ("B", "08:57+3", "")]);
    b.trip("RE 2", &[("B", "", "08:58"), ("D", "09:30", "")]);
    let tt = b.build("A", "D");
    for (missed_by, fastest, expected) in [(0, None, &[][..]), (2, Some(90), &["ICE 1 A-B / RE 2 B-D"])] {
        let limits =
            Limits { missed_by: min(missed_by), extra_time: TimeDelta::zero(), ..limits(&tt, "08:00", "08:00") };
        let result = enumerate(&Network::new(&tt).unwrap(), &limits).unwrap();
        assert_eq!(
            (result.fastest, shown(&tt, &result)),
            (fastest.map(min), sorted(expected)),
            "missed by {missed_by}"
        );
    }
}

#[test]
fn lower_bounds_take_the_fastest_ride_per_stretch() {
    let mut b = Builder::new();
    b.trip("ICE 1", &[("A", "", "08:00+10"), ("B", "09:00-5", "")]); // planned 60, live 45
    b.trip("RE 2", &[("A", "", "09:00"), ("B", "09:50", "")]); // 50
    let re3 = b.trip("RE 3", &[("B", "", "10:00"), ("X", "10:10", "10:10"), ("D", "10:30", "")]);
    b.cancel(re3, "X");
    b.trip("RE 4", &[("X", "", "11:00"), ("D", "11:01", "")]);
    b.trip("RE 5", &[("D", "", "12:00"), ("Q", "12:10", "")]);
    b.walk("W", "A", 3);
    let tt = b.build("A", "D");
    let net = Network::new(&tt).unwrap();
    // (station, planned, never above live)
    let cases = [
        ("D", Some(0), Some(0)),
        ("X", Some(1), Some(1)),
        // B → D 30 min on RE 3: X is skipped, not 10 + 1 via RE 4.
        ("B", Some(30), Some(30)),
        ("A", Some(80), Some(75)),
        ("W", Some(83), Some(78)),
        ("Q", None, None),
    ];
    for (name, planned, live) in cases {
        let s = station(&tt, name);
        assert_eq!((net.lower_bound[s], net.bound[s]), (planned.map(min), live.map(min)), "{name}");
    }
}

/// The last train may leave right at the deadline when its live times are
/// squeezed (+10/+6): RE 2 leaves and arrives 08:30, the fastest arrival.
#[test]
fn a_train_leaving_at_the_deadline_still_counts() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "08:30", "")]);
    b.trip("RE 2", &[("B", "", "08:20+10"), ("D", "08:24+6", "")]);
    let tt = b.build("A", "D");
    let limits = Limits { extra_time: TimeDelta::zero(), ..limits(&tt, "08:00", "08:00") };
    let result = enumerate(&Network::new(&tt).unwrap(), &limits).unwrap();
    assert_eq!((result.fastest, shown(&tt, &result)), (Some(min(30)), vec!["RE 1 A-B / RE 2 B-D".to_owned()]));
}

/// RE 2 runs to X, from where nothing leads to D: the search doesn't stop
/// there, so the effort is the origin and the destination only.
#[test]
fn stations_that_cannot_reach_the_destination_are_not_explored() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("D", "09:00", "")]);
    b.trip("RE 2", &[("A", "", "08:00"), ("X", "08:30", "")]);
    b.trip("RE 3", &[("X", "", "08:40"), ("Y", "09:00", "")]);
    let tt = b.build("A", "D");
    let limits = Limits { max_changes: 0, ..limits(&tt, "08:00", "08:00") };
    let result = enumerate(&Network::new(&tt).unwrap(), &limits).unwrap();
    assert_eq!((shown(&tt, &result), result.nodes), (vec!["RE 1 A-D".to_owned()], 2));
}

/// From Q, RB 4 at 09:00 arrives after ICE 3 at 09:10: waiting is faster,
/// so arriving at Q 08:50 (RE 5 from V) the best is 09:40.
#[test]
fn fastest_journeys_wait_for_a_later_faster_train() {
    let mut b = Builder::new();
    b.trip("ICE 3", &[("Q", "", "09:10"), ("D", "09:40", "")]);
    b.trip("RB 4", &[("Q", "", "09:00"), ("D", "10:00", "")]);
    b.trip("RE 5", &[("V", "", "08:20"), ("Q", "08:50", "")]);
    let tt = b.build("V", "D");
    let fastest = fastest_journeys(&tt).unwrap();
    assert_eq!((fastest[station(&tt, "V")], fastest[station(&tt, "Q")]), (Some(min(80)), Some(min(30))));
}

/// Rides taking no time at 09:00, scanned in an order where ICE 1 passing P
/// → Q comes before RE 2 Q → D that it leads onto: a second pass finds that
/// ICE 1 from V gets to D at 09:00, not by RB 3 at 09:40.
#[test]
fn fastest_journeys_settle_rides_taking_no_time() {
    let mut b = Builder::new();
    b.trip("ICE 1", &[("V", "", "08:30"), ("P", "09:00", ""), ("Q", "09:00", "")]);
    b.trip("RE 2", &[("Q", "", "08:50+10"), ("D", "08:55+5", "")]);
    b.trip("RB 3", &[("Q", "", "09:10"), ("D", "09:40", "")]);
    let tt = b.build("V", "D");
    let fastest = fastest_journeys(&tt).unwrap();
    let at_station = |name| fastest[station(&tt, name)];
    assert_eq!((at_station("V"), at_station("P"), at_station("Q")), (Some(min(30)), None, Some(TimeDelta::zero())));
}

#[test]
fn a_running_search_reports_rounds_states_and_finds() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "08:30", "")]);
    b.trip("RE 2", &[("B", "", "08:40"), ("C", "09:10", "")]);
    b.trip("RE 3", &[("A", "", "08:10"), ("C", "09:30", "")]);
    let tt = b.build("A", "C");
    let reports = std::sync::Mutex::new(Vec::new());
    let result = enumerate_reporting(&Network::new(&tt).unwrap(), &limits(&tt, "07:00", "09:00"), &|p| {
        reports.lock().unwrap().push(p);
    })
    .unwrap();
    let reports = reports.into_inner().unwrap();
    // Tests report at every state.
    assert_eq!(reports.len(), result.nodes);
    assert!(
        reports.windows(2).all(|w| w[0].nodes < w[1].nodes && w[0].changes <= w[1].changes && w[0].found <= w[1].found)
    );
    let last = reports.last().unwrap();
    assert_eq!((last.nodes, last.changes), (result.nodes, 5));
    // Counted when a state is entered, before it may record one more.
    assert!(last.found <= result.chains.len() && reports.iter().any(|p| p.changes == 1 && p.found >= 1));
}

/// Every connection the rules allow, by plain depth-first search without
/// pruning, indexes or deepening; detours kept.
struct Oracle<'a> {
    tt: &'a Timetable,
    limits: &'a Limits,
    max_duration: TimeDelta,
    lower_bound: &'a [Option<TimeDelta>],
    visited: Vec<bool>,
    used: Vec<bool>,
    steps: Vec<Step>,
    found: HashSet<Vec<Step>>,
}

impl Oracle<'_> {
    fn chains(
        tt: &Timetable,
        limits: &Limits,
        max_duration: TimeDelta,
        lower_bound: &[Option<TimeDelta>],
    ) -> HashSet<Vec<Step>> {
        let mut o = Oracle {
            tt,
            limits,
            max_duration,
            lower_bound,
            visited: vec![false; tt.stations.len()],
            used: vec![false; tt.trips.len()],
            steps: Vec::new(),
            found: HashSet::new(),
        };
        o.visited[limits.origin] = true;
        o.go(limits.origin, limits.depart_from, None, 0, false);
        o.found
    }

    fn go(
        &mut self,
        station: usize,
        ready: DateTime<Utc>,
        deadline: Option<DateTime<Utc>>,
        rides: usize,
        walked: bool,
    ) {
        let tt = self.tt;
        if station == tt.destination {
            self.found.insert(self.steps.clone());
            return;
        }
        if let Some(deadline) = deadline.filter(|_| !walked) {
            for (i, fp) in tt.footpaths.iter().enumerate().filter(|(_, fp)| fp.from == station) {
                if self.visited[fp.to] || ready + fp.duration > deadline {
                    continue;
                }
                self.visited[fp.to] = true;
                self.steps.push(Step::Walk { footpath: i });
                self.go(fp.to, ready + fp.duration, Some(deadline), rides, true);
                self.steps.pop();
                self.visited[fp.to] = false;
            }
        }
        if rides > self.limits.max_changes {
            return;
        }
        for (trip, t) in tt.trips.iter().enumerate() {
            if self.used[trip] {
                continue;
            }
            for (board, call) in t.calls.iter().enumerate() {
                let Some(departure) = call.departure_time().filter(|_| call.station == station && !call.cancelled)
                else {
                    continue;
                };
                let deadline = match deadline {
                    None if (self.limits.depart_from..=self.limits.depart_until).contains(&departure) => {
                        departure + self.max_duration
                    }
                    Some(d) if departure >= ready - self.limits.missed_by => d,
                    _ => continue,
                };
                self.used[trip] = true;
                let mut passed = Vec::new();
                for (alight, c) in t.calls.iter().enumerate().skip(board + 1) {
                    let Some(arrival) = c.arrival_time() else { break };
                    if self.visited[c.station] || arrival < departure || arrival > deadline {
                        break;
                    }
                    self.visited[c.station] = true;
                    passed.push(c.station);
                    let progress = !self.limits.forward_only || self.lower_bound[c.station] < self.lower_bound[station];
                    if !c.cancelled && progress {
                        self.steps.push(Step::Ride { trip, board, alight });
                        self.go(c.station, arrival, Some(deadline), rides + 1, false);
                        self.steps.pop();
                    }
                }
                for s in passed {
                    self.visited[s] = false;
                }
                self.used[trip] = false;
            }
        }
    }
}

/// Fastest journey from each station by brute force: any trains, waits and
/// walks (one before the first train, never two in a row), revisits allowed.
/// Returns (duration, planned ride and walk time, live ride and walk time),
/// each minimised on its own.
fn journeys(tt: &Timetable, from: usize) -> [Option<TimeDelta>; 3] {
    struct Best {
        tt_destination: usize,
        best: [Option<TimeDelta>; 3],
    }
    impl Best {
        fn offer(&mut self, values: [TimeDelta; 3]) {
            for (b, v) in self.best.iter_mut().zip(values) {
                *b = Some(b.map_or(v, |b| b.min(v)));
            }
        }
    }
    /// At `station` at `ready` (`None` before the first train), having left at
    /// `start` after walking `lead` first; `rode` is (planned, live) time moving.
    #[expect(clippy::too_many_arguments, reason = "the search state, spelled out")]
    fn go(
        tt: &Timetable,
        b: &mut Best,
        used: &mut [bool],
        station: usize,
        ready: Option<DateTime<Utc>>,
        start: Option<DateTime<Utc>>,
        lead: TimeDelta,
        rode: (TimeDelta, TimeDelta),
        walked: bool,
    ) {
        if station == b.tt_destination {
            let duration = match (start, ready) {
                (Some(start), Some(ready)) => ready - start,
                _ => lead,
            };
            b.offer([duration, rode.0, rode.1]);
            return;
        }
        if !walked {
            for fp in tt.footpaths.iter().filter(|fp| fp.from == station) {
                let lead = if start.is_none() { lead + fp.duration } else { lead };
                let rode = (rode.0 + fp.duration, rode.1 + fp.duration);
                go(tt, b, used, fp.to, ready.map(|r| r + fp.duration), start, lead, rode, true);
            }
        }
        for (trip, t) in tt.trips.iter().enumerate() {
            if used[trip] {
                continue;
            }
            for (board, call) in t.calls.iter().enumerate() {
                let Some(departure) = call.departure.filter(|_| call.station == station && !call.cancelled) else {
                    continue;
                };
                if ready.is_some_and(|r| departure.actual < r) {
                    continue;
                }
                used[trip] = true;
                let start = start.or(Some(departure.actual - lead));
                for c in t.calls.iter().skip(board + 1).filter(|c| !c.cancelled) {
                    let Some(arrival) = c.arrival.or(c.departure) else { continue };
                    let rode =
                        (rode.0 + (arrival.planned - departure.planned), rode.1 + (arrival.actual - departure.actual));
                    go(tt, b, used, c.station, Some(arrival.actual), start, lead, rode, false);
                }
                used[trip] = false;
            }
        }
    }
    let mut b = Best { tt_destination: tt.destination, best: [None; 3] };
    let zero = TimeDelta::zero();
    go(tt, &mut b, &mut vec![false; tt.trips.len()], from, None, None, zero, (zero, zero), false);
    b.best
}

fn arb_limits() -> impl Strategy<Value = (i64, i64, usize, i64, i64, bool)> {
    (0i64..=60, 0i64..=90, 0usize..=4, 0i64..=90, 0i64..=10, any::<bool>())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Exactly the connections within the limits, each once; pruning detours
    /// only removes connections.
    #[test]
    fn enumeration_matches_brute_force(
        spec in spec(true, true),
        (from, window, max_changes, extra, missed_by, forward_only) in arb_limits(),
    ) {
        let tt = timetable(&spec);
        let net = Network::new(&tt).unwrap();
        let depart_from = at("08:00") + min(from);
        let limits = Limits {
            origin: tt.origin,
            depart_from,
            depart_until: depart_from + min(window),
            max_changes,
            extra_time: min(extra),
            min_transfer: min(5),
            missed_by: min(missed_by),
            keep_detours: true,
            forward_only,
            max_nodes: usize::MAX,
        };
        let result = enumerate(&net, &limits).unwrap();
        let got: Vec<Vec<Step>> = result.chains.iter().map(|c| c.steps.clone()).collect();
        let unique: HashSet<Vec<Step>> = got.iter().cloned().collect();
        prop_assert_eq!(unique.len(), got.len(), "listed twice");
        prop_assert_eq!(result.complete_up_to, Some(max_changes));
        // Without a fastest connection there is none at all, whatever the duration.
        let max_duration = result.fastest.map_or(TimeDelta::days(10), |f| f + limits.extra_time);
        let expected = Oracle::chains(&tt, &limits, max_duration, &net.lower_bound);
        let show_all = |set: &HashSet<Vec<Step>>| {
            let mut v: Vec<String> = set.iter().map(|s| show(&tt, s)).collect();
            v.sort();
            v
        };
        prop_assert_eq!(show_all(&unique), show_all(&expected));

        let pruned = enumerate(&net, &Limits { keep_detours: false, ..limits }).unwrap();
        prop_assert!(pruned.chains.iter().all(|c| unique.contains(&c.steps)));
    }

    /// `fastest` is the fastest connection that can be listed; if only missed
    /// ones exist, the fastest of those. (Earliest arrival can't see that the
    /// enumeration never revisits a station, which matters only after passing
    /// a stop without leaving the train there, or arriving on foot where a
    /// walk later leaves: no gaps, no trains back through the origin, walks
    /// only into the destination here.)
    #[test]
    fn fastest_is_the_fastest_listed_connection(
        spec in spec(false, false),
        (from, window, _, _, missed_by, _) in arb_limits(),
    ) {
        let destination = spec.stations - 1;
        let spec = Spec { walks: spec.walks.into_iter().filter(|w| w.1 == destination).collect(), ..spec };
        let tt = timetable(&spec);
        let net = Network::new(&tt).unwrap();
        let depart_from = at("08:00") + min(from);
        let limits = Limits {
            depart_from,
            depart_until: depart_from + min(window),
            max_changes: 20,
            keep_detours: true,
            ..limits(&tt, "08:00", "08:00")
        };
        let fastest = |limits: &Limits| {
            Oracle::chains(&tt, limits, TimeDelta::days(10), &net.lower_bound).into_iter().map(|steps| {
                let chain = Chain { steps };
                chain.arrival(&tt).unwrap() - chain.departure(&tt).unwrap()
            }).min()
        };
        let missed = Limits { missed_by: min(missed_by), ..limits };
        let expected = fastest(&limits).or_else(|| fastest(&missed));
        prop_assert_eq!(enumerate(&net, &missed).unwrap().fastest, expected);
    }

    /// Cutting the search short leaves the connections with up to
    /// `complete_up_to` changes complete, and lists nothing extra.
    #[test]
    fn truncated_search_is_complete_up_to_what_it_says(spec in spec(true, true), cap in 0usize..400) {
        let tt = timetable(&spec);
        let net = Network::new(&tt).unwrap();
        let limits = Limits { max_changes: 4, keep_detours: true, ..limits(&tt, "08:00", "09:30") };
        let full = enumerate(&net, &limits).unwrap();
        let cut = enumerate(&net, &Limits { max_nodes: cap, ..limits }).unwrap();
        let full_set: HashSet<Vec<Step>> = full.chains.iter().map(|c| c.steps.clone()).collect();
        prop_assert!(cut.chains.iter().all(|c| full_set.contains(&c.steps)));
        if let Some(k) = cut.complete_up_to {
            let upto = |chains: &[Chain]| {
                chains.iter().filter(|c| c.changes() <= k).map(|c| c.steps.clone()).collect::<HashSet<_>>()
            };
            prop_assert_eq!(upto(&cut.chains), upto(&full.chains));
        }
    }

    /// `fastest_journeys` is the fastest real journey from each station; the
    /// lower bounds never exceed the time spent moving on any journey.
    #[test]
    fn per_station_bounds_match_brute_force(spec in spec(true, true)) {
        let tt = timetable(&spec);
        let net = Network::new(&tt).unwrap();
        let fastest = fastest_journeys(&tt).unwrap();
        for (s, &fastest) in fastest.iter().enumerate() {
            let [duration, planned, live] = journeys(&tt, s);
            prop_assert_eq!(fastest, duration, "fastest from S{}", s);
            if let Some(planned) = planned {
                prop_assert!(net.lower_bound[s].is_some_and(|lb| lb <= planned), "planned bound at S{}", s);
            }
            if let Some(live) = live {
                prop_assert!(net.bound[s].is_some_and(|lb| lb <= live), "live bound at S{}", s);
            }
            prop_assert!(net.bound[s] <= net.lower_bound[s]);
        }
    }
}
