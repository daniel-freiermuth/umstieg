use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::UTC;

use super::*;
use crate::enumerate::Step;
use crate::testkit::{Builder, at, ride, show};
use crate::timetable::Timetable;

const MIN_TRANSFER: TimeDelta = TimeDelta::minutes(5);

/// Hamburg Hbf around RJ 384 (12:00), which plans change to.
fn hamburg() -> Timetable {
    let mut b = Builder::new();
    b.trip("ICE 2", &[("Frankfurt", "", "08:00"), ("Hamburg", "11:51", "")]);
    b.trip("IC 5", &[("Kiel", "", "10:30"), ("Hamburg", "11:40", "")]);
    b.trip(
        "RJ 384",
        &[
            ("Hamburg", "", "12:00"),
            ("Altona", "12:20", "12:25"),
            ("Schleswig", "13:30", "13:32"),
            ("København", "17:00", ""),
        ],
    );
    b.trip("S 1", &[("Hamburg", "", "12:05"), ("Altona", "12:15", "")]);
    b.trip("S 3", &[("Hamburg", "", "12:10"), ("Altona", "12:26", "")]); // RJ 384 gone by a minute
    b.trip("RE 7", &[("Hamburg", "", "12:01"), ("Schleswig", "13:00", "")]);
    b.trip("RE 9", &[("Schleswig", "", "13:40"), ("København", "17:30", "")]);
    b.trip("IC 1", &[("Hamburg", "", "12:00"), ("København", "16:00", "")]); // leaves with the missed train
    b.trip("RJ 386", &[("Hamburg", "", "14:01"), ("København", "19:00", "")]);
    b.trip("RJ 388", &[("Hamburg", "", "14:02"), ("København", "18:10", "")]);
    // A plan changing at Altona at 12:00 too.
    b.trip("RB 10", &[("Kiel", "", "10:00"), ("Altona", "11:45", "")]);
    b.trip("RE 11", &[("Altona", "", "12:00"), ("København", "16:50", "")]);
    b.build("Frankfurt", "København")
}

fn plan(tt: &Timetable, rides: &[(&str, &str, &str)]) -> Chain {
    Chain { steps: rides.iter().map(|&(label, from, to)| ride(tt, label, from, to)).collect() }
}

/// The fallback up to where it is back on plan, and its arrival.
fn summary(tt: &Timetable, f: &Fallback) -> (String, DateTime<Utc>) {
    assert_eq!(f.line.contains("(back on plan)"), f.rejoins_at.is_some(), "{}", f.line);
    let rides: Vec<_> = f.chain.rides().collect();
    let name = |trip: usize, call: usize| tt.stations[tt.trips[trip].calls[call].station].name.as_str();
    let parts: Vec<String> = rides
        .iter()
        .enumerate()
        .take(f.rejoins_at.map_or(rides.len(), |k| k.saturating_add(1)))
        .map(|(i, &(trip, board, alight))| {
            let label = &tt.trips[trip].label;
            if Some(i) == f.rejoins_at {
                format!("{label} from {} (back on plan)", name(trip, board))
            } else {
                format!("{label} {}-{}", name(trip, board), name(trip, alight))
            }
        })
        .collect();
    (parts.join(" / "), f.lands)
}

fn summaries(tt: &Timetable, fallbacks: &[Fallback]) -> Vec<(String, DateTime<Utc>)> {
    fallbacks.iter().map(|f| summary(tt, f)).collect()
}

fn expect(rows: &[(&str, &str)]) -> Vec<(String, DateTime<Utc>)> {
    rows.iter().map(|&(s, t)| (s.to_owned(), at(t))).collect()
}

/// From the change station, leaving after the missed train (not with it)
/// up to two hours later, only what still works; detours pruned.
#[test]
fn candidates_leave_after_the_missed_train_within_the_window() {
    let tt = hamburg();
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let hamburg = tt.stations.iter().position(|s| s.name == "Hamburg").unwrap();
    let mut got: Vec<String> =
        fallbacks.candidates(hamburg, at("12:00"), at("11:51")).unwrap().iter().map(|c| show(&tt, &c.steps)).collect();
    got.sort();
    let mut expected = vec![
        "RE 7 Hamburg-Schleswig / RJ 384 Schleswig-København",
        "RE 7 Hamburg-Schleswig / RE 9 Schleswig-København",
        "S 1 Hamburg-Altona / RJ 384 Altona-København",
        "S 1 Hamburg-Altona / RJ 384 Altona-Schleswig / RE 9 Schleswig-København",
        "RJ 386 Hamburg-København", // 14:01, the window's end
    ];
    expected.sort_unstable();
    assert_eq!(got, expected);
}

/// Ranked by arrival, then changes and buffer until back on plan; the plan's
/// arrival counts once a train of it is caught. Identical up to there: shown once.
#[test]
fn fallbacks_rank_by_landing_and_end_back_on_plan() {
    let tt = hamburg();
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let p = plan(&tt, &[("ICE 2", "Frankfurt", "Hamburg"), ("RJ 384", "Hamburg", "København")]);
    assert_eq!(
        summaries(&tt, &fallbacks.for_change(&p, 0).unwrap()),
        expect(&[
            // 32 min at Schleswig beats 10 at Altona.
            ("RE 7 Hamburg-Schleswig / RJ 384 from Schleswig (back on plan)", "17:00"),
            ("S 1 Hamburg-Altona / RJ 384 from Altona (back on plan)", "17:00"),
            ("RE 7 Hamburg-Schleswig / RE 9 Schleswig-København", "17:30"),
        ])
    );
}

/// The same fallbacks for a plan that leaves RJ 384 at Schleswig for RE 9:
/// boarding RJ 384 there is not back on plan, and lands earlier than it.
#[test]
fn rejoining_counts_only_before_the_plan_leaves_the_train() {
    let tt = hamburg();
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let p = plan(
        &tt,
        &[("IC 5", "Kiel", "Hamburg"), ("RJ 384", "Hamburg", "Schleswig"), ("RE 9", "Schleswig", "København")],
    );
    assert_eq!(
        summaries(&tt, &fallbacks.for_change(&p, 0).unwrap()),
        expect(&[
            ("RE 7 Hamburg-Schleswig / RJ 384 Schleswig-København", "17:00"),
            ("RE 7 Hamburg-Schleswig / RE 9 from Schleswig (back on plan)", "17:30"),
            ("S 1 Hamburg-Altona / RJ 384 from Altona (back on plan)", "17:30"),
        ])
    );
}

/// ICE 2 is overtaken; the plan leaves it at C for ICE 3. Catching ICE 2
/// again at E (or at C, as it leaves) is not back on plan: it gets in at 10:40.
#[test]
fn catching_a_plan_train_after_the_plan_left_it_lands_on_its_own() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "08:50", "")]);
    b.trip("ICE 2", &[("B", "", "09:00"), ("C", "09:30", "09:31"), ("E", "10:00", "10:01"), ("D", "10:40", "")]);
    b.trip("ICE 3", &[("C", "", "09:40"), ("D", "10:30", "")]);
    b.trip("RB 4", &[("B", "", "09:10"), ("E", "09:45", "")]);
    b.trip("RB 5", &[("B", "", "09:05"), ("C", "09:20", "")]);
    let tt = b.build("A", "D");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let p = plan(&tt, &[("RE 1", "A", "B"), ("ICE 2", "B", "C"), ("ICE 3", "C", "D")]);
    assert_eq!(
        summaries(&tt, &fallbacks.for_change(&p, 0).unwrap()),
        expect(&[
            ("RB 5 B-C / ICE 3 from C (back on plan)", "10:30"),
            ("RB 4 B-E / ICE 2 E-D", "10:40"), // 16 min at E
            ("RB 5 B-C / ICE 2 C-D", "10:40"), // 11 min at C
        ])
    );
}

/// Candidates are computed once per (station, missed departure) and serve
/// every plan changing there; plans at other stations get their own.
#[test]
fn cached_candidates_serve_each_plan_like_fresh_ones() {
    let tt = hamburg();
    let net = Network::new(&tt).unwrap();
    let plans = [
        plan(&tt, &[("ICE 2", "Frankfurt", "Hamburg"), ("RJ 384", "Hamburg", "København")]),
        plan(
            &tt,
            &[("IC 5", "Kiel", "Hamburg"), ("RJ 384", "Hamburg", "Schleswig"), ("RE 9", "Schleswig", "København")],
        ),
        // Also missing a 12:00 departure, at Altona.
        plan(&tt, &[("RB 10", "Kiel", "Altona"), ("RE 11", "Altona", "København")]),
    ];
    let mut shared = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let mut seen = Vec::new();
    for p in plans.iter().chain(&plans) {
        let got = summaries(&tt, &shared.for_change(p, 0).unwrap());
        assert_eq!(got, summaries(&tt, &Fallbacks::new(&net, MIN_TRANSFER, UTC).for_change(p, 0).unwrap()));
        seen.push(got);
    }
    assert!(seen[0] != seen[1] && seen[0] != seen[2] && seen[1] != seen[2], "{seen:#?}");
    assert_eq!(shared.cache.len(), 2);
}

#[test]
fn nothing_within_the_window_means_no_fallbacks() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "08:50", "")]);
    b.trip("RE 2", &[("B", "", "09:00"), ("D", "09:30", "")]);
    b.trip("RE 3", &[("B", "", "11:02"), ("D", "11:30", "")]); // 2h01 after the missed one
    let tt = b.build("A", "D");
    let net = Network::new(&tt).unwrap();
    let p = plan(&tt, &[("RE 1", "A", "B"), ("RE 2", "B", "D")]);
    assert!(Fallbacks::new(&net, MIN_TRANSFER, UTC).for_change(&p, 0).unwrap().is_empty());
}

#[test]
fn a_change_the_plan_does_not_have_is_an_error() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "08:50", "")]);
    b.trip("RE 2", &[("B", "", "09:00"), ("D", "09:30", "")]);
    let tt = b.build("A", "D");
    let net = Network::new(&tt).unwrap();
    let p = plan(&tt, &[("RE 1", "A", "B"), ("RE 2", "B", "D")]);
    let err = Fallbacks::new(&net, MIN_TRANSFER, UTC).for_change(&p, 1).err().unwrap().to_string();
    assert_eq!(err, "the connection has no change 1");
}

#[test]
fn after_a_change_with_a_walk_fallbacks_also_leave_from_where_the_walk_led() {
    // Tram to the stop outside the station, walk 8 min, ICE 1; missing ICE 1
    // leaves the later ICE 3 from the station, which the tram stop doesn't serve.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("ICE 2", &[("Hbf", "", "09:05"), ("Ziel", "10:05", "")]);
    b.trip("ICE 3", &[("Hbf", "", "09:30"), ("Ziel", "10:30", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let walk = tt.footpaths.iter().position(|f| tt.stations[f.from].name == "Zentrum").unwrap();
    let chain = Chain {
        steps: vec![
            ride(&tt, "STR 4", "Dorf", "Zentrum"),
            Step::Walk { footpath: walk },
            ride(&tt, "ICE 1", "Hbf", "Ziel"),
        ],
    };
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let found: Vec<_> = fallbacks.for_change(&chain, 0).unwrap().iter().map(|f| summary(&tt, f)).collect();
    // ICE 2 leaves within the walk's length after ICE 1, yet the traveller is at Hbf.
    assert_eq!(found, [("ICE 2 Hbf-Ziel".to_owned(), at("10:05")), ("ICE 3 Hbf-Ziel".to_owned(), at("10:30"))]);
}

#[test]
fn after_a_change_with_a_walk_a_fallback_is_listed_once() {
    // The walk goes both ways: from Hbf the tram is found again behind a walk back.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.walk("Hbf", "Zentrum", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("STR 9", &[("Zentrum", "", "09:20"), ("Ziel", "10:40", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let walk = tt.footpaths.iter().position(|f| tt.stations[f.from].name == "Zentrum").unwrap();
    let chain = Chain {
        steps: vec![
            ride(&tt, "STR 4", "Dorf", "Zentrum"),
            Step::Walk { footpath: walk },
            ride(&tt, "ICE 1", "Hbf", "Ziel"),
        ],
    };
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let found: Vec<_> = fallbacks.for_change(&chain, 0).unwrap().iter().map(|f| f.line.clone()).collect();
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn after_a_change_with_a_walk_a_fallback_walked_to_at_the_window_end_stays() {
    // ICE 1 missed at 09:00: the window from Hbf ends at 11:01, but walking
    // from Zentrum at 10:57 still catches ICE 5 at 11:05.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("ICE 5", &[("Hbf", "", "11:05"), ("Ziel", "12:05", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let walk = tt.footpaths.iter().position(|f| tt.stations[f.from].name == "Zentrum").unwrap();
    let chain = Chain {
        steps: vec![
            ride(&tt, "STR 4", "Dorf", "Zentrum"),
            Step::Walk { footpath: walk },
            ride(&tt, "ICE 1", "Hbf", "Ziel"),
        ],
    };
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let found: Vec<_> = fallbacks.for_change(&chain, 0).unwrap().iter().map(|f| summary(&tt, f)).collect();
    assert_eq!(found, [("ICE 5 Hbf-Ziel".to_owned(), at("12:05"))]);
}

#[test]
fn after_a_change_with_a_walk_fallbacks_walk_only_from_where_the_train_arrived() {
    // Fallbacks are shown from Zentrum: one walking from Hbf to the bus
    // station or back to Zentrum would be drawn and listed as a walk from Zentrum.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.walk("Hbf", "Zentrum", 8);
    b.walk("Hbf", "ZOB", 3);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("ICE 2", &[("Hbf", "", "09:05"), ("Ziel", "10:05", "")]);
    b.trip("BUS 7", &[("ZOB", "", "09:06"), ("Ziel", "10:10", "")]);
    b.trip("STR 9", &[("Zentrum", "", "11:05"), ("Ziel", "11:40", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let walk = tt.footpaths.iter().position(|f| tt.stations[f.from].name == "Zentrum").unwrap();
    let chain = Chain {
        steps: vec![
            ride(&tt, "STR 4", "Dorf", "Zentrum"),
            Step::Walk { footpath: walk },
            ride(&tt, "ICE 1", "Hbf", "Ziel"),
        ],
    };
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let found: Vec<_> = fallbacks.for_change(&chain, 0).unwrap().iter().map(|f| summary(&tt, f)).collect();
    assert_eq!(found, [("ICE 2 Hbf-Ziel".to_owned(), at("10:05"))]);
}

#[test]
fn after_a_change_with_a_walk_a_train_calling_at_both_stations_is_listed_once() {
    // RE 6 calls at Zentrum, then at Hbf: boarded at Zentrum, it is one fallback.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("RE 6", &[("Zentrum", "", "09:20"), ("Hbf", "09:24", "09:25"), ("Ziel", "10:30", "")]);
    b.trip("ICE 3", &[("Hbf", "", "09:40"), ("Ziel", "10:40", "")]);
    b.trip("ICE 7", &[("Hbf", "", "09:50"), ("Ziel", "10:50", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let walk = tt.footpaths.iter().position(|f| tt.stations[f.from].name == "Zentrum").unwrap();
    let chain = Chain {
        steps: vec![
            ride(&tt, "STR 4", "Dorf", "Zentrum"),
            Step::Walk { footpath: walk },
            ride(&tt, "ICE 1", "Hbf", "Ziel"),
        ],
    };
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    assert_eq!(
        summaries(&tt, &fallbacks.for_change(&chain, 0).unwrap()),
        expect(&[
            ("RE 6 Zentrum-Ziel", "10:30"),
            ("ICE 3 Hbf-Ziel", "10:40"),
            ("RE 6 Zentrum-Hbf / ICE 3 Hbf-Ziel", "10:40"),
        ])
    );
}

/// STR 4 to Zentrum, the walk to Hbf, ICE 1 from there.
fn walked_change(tt: &Timetable) -> Chain {
    let named = |s: usize, name: &str| tt.stations[s].name == name;
    let walk = tt.footpaths.iter().position(|f| named(f.from, "Zentrum") && named(f.to, "Hbf")).unwrap();
    Chain {
        steps: vec![
            ride(tt, "STR 4", "Dorf", "Zentrum"),
            Step::Walk { footpath: walk },
            ride(tt, "ICE 1", "Hbf", "Ziel"),
        ],
    }
}

#[test]
fn after_a_change_with_a_walk_a_fallback_from_the_other_station_names_it() {
    // Listed under the change at Zentrum: ICE 3 leaves Hbf, STR 9 Zentrum.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("ICE 3", &[("Hbf", "", "09:30"), ("Ziel", "10:30", "")]);
    b.trip("STR 9", &[("Zentrum", "", "09:20"), ("Ziel", "10:40", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let lines: Vec<_> = fallbacks.for_change(&walked_change(&tt), 0).unwrap().into_iter().map(|f| f.line).collect();
    assert_eq!(lines, ["09:30 UTC Hbf ICE 3 → 10:30 UTC Ziel", "09:20 UTC STR 9 → 10:40 UTC Ziel"]);
}

#[test]
fn after_a_change_with_a_walk_a_fallback_riding_back_to_walk_on_is_no_second_one() {
    // RB 8 back to Zentrum, then the walk to W for a bus, is that bus from Zentrum again.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.walk("Zentrum", "W", 3);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("RB 8", &[("Hbf", "", "09:03"), ("Zentrum", "09:07", "")]);
    b.trip("BUS 1", &[("W", "", "09:20"), ("Ziel", "10:40", "")]);
    b.trip("BUS 2", &[("W", "", "09:30"), ("Ziel", "10:50", "")]);
    b.trip("BUS 3", &[("W", "", "09:40"), ("Ziel", "11:00", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let lines: Vec<_> = fallbacks.for_change(&walked_change(&tt), 0).unwrap().into_iter().map(|f| f.line).collect();
    assert_eq!(
        lines,
        [
            "walk 3m to W 09:20 UTC BUS 1 → 10:40 UTC Ziel",
            "walk 3m to W 09:30 UTC BUS 2 → 10:50 UTC Ziel",
            "walk 3m to W 09:40 UTC BUS 3 → 11:00 UTC Ziel",
        ]
    );
}

#[test]
fn after_a_change_with_a_walk_a_ride_back_to_a_train_past_the_window_stays() {
    // RB 8 leaves Hbf within the window; STR 9 leaves Zentrum after it, so
    // the search from Zentrum doesn't list it.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("RB 8", &[("Hbf", "", "10:58"), ("Zentrum", "11:02", "")]);
    b.trip("STR 9", &[("Zentrum", "", "11:05"), ("Ziel", "11:40", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    assert_eq!(
        summaries(&tt, &fallbacks.for_change(&walked_change(&tt), 0).unwrap()),
        expect(&[("RB 8 Hbf-Zentrum / STR 9 Zentrum-Ziel", "11:40")])
    );
}

/// The walked change, a walk from Zentrum to W, and BUS 5 calling at W, then at Hbf.
fn bus_through_w_and_hbf(w: &str, hbf: &str, ziel: &str) -> Vec<String> {
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.walk("Zentrum", "W", 3);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("BUS 5", &[("W", "", w), ("Hbf", hbf, hbf), ("Ziel", ziel, "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    fallbacks.for_change(&walked_change(&tt), 0).unwrap().into_iter().map(|f| f.line).collect()
}

#[test]
fn after_a_change_with_a_walk_a_train_walked_to_elsewhere_is_also_boarded_at_hbf() {
    // Walking from Zentrum to W, or boarding at Hbf: two places to catch BUS 5.
    assert_eq!(
        bus_through_w_and_hbf("09:20", "09:22", "10:30"),
        ["09:22 UTC Hbf BUS 5 → 10:30 UTC Ziel", "walk 3m to W 09:20 UTC BUS 5 → 10:30 UTC Ziel"]
    );
}

#[test]
fn after_a_change_with_a_walk_a_train_at_hbf_right_after_the_missed_one_stays() {
    // BUS 5 leaves Hbf at 09:06, in the walk's length after ICE 1: the
    // traveller who missed it there can't make the walk to W for 09:04.
    assert_eq!(
        bus_through_w_and_hbf("09:04", "09:06", "10:10"),
        ["09:06 UTC Hbf BUS 5 → 10:10 UTC Ziel", "walk 3m to W 09:04 UTC BUS 5 → 10:10 UTC Ziel"]
    );
}

#[test]
fn after_a_change_with_a_walk_a_fallback_riding_through_is_no_second_one() {
    // RB 8 to S, then STR 9 through Zentrum: from there it is STR 9 from Zentrum again.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("RB 8", &[("Hbf", "", "09:03"), ("S", "09:08", "")]);
    b.trip("STR 9", &[("S", "", "09:14"), ("Zentrum", "09:20", "09:20"), ("Ziel", "10:40", "")]);
    b.trip("BUS 1", &[("Zentrum", "", "09:30"), ("Ziel", "10:50", "")]);
    b.trip("BUS 2", &[("Zentrum", "", "09:40"), ("Ziel", "11:00", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    assert_eq!(
        summaries(&tt, &fallbacks.for_change(&walked_change(&tt), 0).unwrap()),
        expect(&[("STR 9 Zentrum-Ziel", "10:40"), ("BUS 1 Zentrum-Ziel", "10:50"), ("BUS 2 Zentrum-Ziel", "11:00")])
    );
}

#[test]
fn after_a_change_with_a_walk_a_fallback_riding_back_is_no_second_one() {
    // RB 8 back to Zentrum for STR 9 is STR 9 from Zentrum again.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("RB 8", &[("Hbf", "", "09:03"), ("Zentrum", "09:07", "")]);
    b.trip("STR 9", &[("Zentrum", "", "09:20"), ("Ziel", "10:40", "")]);
    b.trip("BUS 1", &[("Zentrum", "", "09:30"), ("Ziel", "10:50", "")]);
    b.trip("BUS 2", &[("Zentrum", "", "09:40"), ("Ziel", "11:00", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    assert_eq!(
        summaries(&tt, &fallbacks.for_change(&walked_change(&tt), 0).unwrap()),
        expect(&[("STR 9 Zentrum-Ziel", "10:40"), ("BUS 1 Zentrum-Ziel", "10:50"), ("BUS 2 Zentrum-Ziel", "11:00")])
    );
}

#[test]
fn after_a_change_with_a_walk_a_train_calling_at_hbf_first_is_boarded_there() {
    // RE 6 leaves Hbf at 09:02, right after ICE 1; at Zentrum, 09:06, it is out of reach from Hbf.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("RE 6", &[("Hbf", "", "09:02"), ("Zentrum", "09:06", "09:06"), ("Ziel", "10:30", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    assert_eq!(
        summaries(&tt, &fallbacks.for_change(&walked_change(&tt), 0).unwrap()),
        expect(&[("RE 6 Hbf-Ziel", "10:30")])
    );
}

#[test]
fn after_a_change_with_a_walk_the_same_trains_changed_elsewhere_stay_two_fallbacks() {
    // X to S, then Y: boarded at S with 2 minutes, or at T after a walk with 7.
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.walk("S", "T", 3);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("X", &[("Zentrum", "", "09:10"), ("S", "09:30", "")]);
    b.trip("Y", &[("S", "", "09:32"), ("T", "09:40", "09:40"), ("Ziel", "10:30", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let lines: Vec<_> = fallbacks.for_change(&walked_change(&tt), 0).unwrap().into_iter().map(|f| f.line).collect();
    assert_eq!(
        lines,
        [
            "09:10 UTC X → 09:30 UTC S ‖7m walk 3m‖ 09:40 UTC T Y → 10:30 UTC Ziel",
            "09:10 UTC X → 09:30 UTC S ‖2m⚠‖ 09:32 UTC Y → 10:30 UTC Ziel",
        ]
    );
}

/// A change listed although missed (`--missed-by`): the fallbacks leave
/// once the traveller is there, not right after the missed train.
#[test]
fn fallbacks_of_a_missed_change_leave_after_the_arrival() {
    let mut b = Builder::new();
    b.trip("ICE 2", &[("Frankfurt", "", "08:00"), ("Hamburg", "11:51+13", "")]);
    b.trip("RJ 384", &[("Hamburg", "", "12:00"), ("Schleswig", "13:30", "13:32"), ("København", "17:00", "")]);
    b.trip("RE 7", &[("Hamburg", "", "12:01"), ("Schleswig", "13:00", "")]);
    b.trip("S 1", &[("Hamburg", "", "12:05"), ("Schleswig", "13:20", "")]);
    let tt = b.build("Frankfurt", "København");
    let net = Network::new(&tt).unwrap();
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let p = plan(&tt, &[("ICE 2", "Frankfurt", "Hamburg"), ("RJ 384", "Hamburg", "København")]);
    assert_eq!(
        summaries(&tt, &fallbacks.for_change(&p, 0).unwrap()),
        expect(&[("S 1 Hamburg-Schleswig / RJ 384 from Schleswig (back on plan)", "17:00")])
    );
}

/// The fallbacks of every change a connection on the network can make: at
/// Hamburg after ICE 2, and wherever RJ 384 stands (Altona, Schleswig), also
/// when arriving a little late (missed by up to `missed_by`); nothing at the
/// origin, where nothing arrives.
#[test]
fn every_change_on_the_network_gets_its_fallbacks() {
    let tt = hamburg();
    let net = Network::new(&tt).unwrap();
    let p = plan(&tt, &[("ICE 2", "Frankfurt", "Hamburg"), ("RJ 384", "Hamburg", "København")]);
    let network = crate::usable::of_chains(&tt, [&p]);
    let station = |name: &str| tt.stations.iter().position(|s| s.name == name).unwrap();
    let shows = |chains: &[Chain]| {
        let mut out: Vec<String> = chains.iter().map(|c| show(&tt, &c.steps)).collect();
        out.sort();
        out.dedup();
        out
    };
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let all = fallbacks.for_network(&network, TimeDelta::minutes(5)).unwrap();
    let mut expected = Vec::new();
    for (name, missed, ready) in
        [("Hamburg", "12:00", "11:51"), ("Altona", "12:25", "12:20"), ("Schleswig", "13:32", "13:30")]
    {
        expected.extend(fallbacks.candidates(station(name), at(missed), at(ready)).unwrap().to_vec());
    }
    assert!(!expected.is_empty());
    assert_eq!(shows(&all), shows(&expected));

    // ICE 2 arriving at 12:04, four minutes after RJ 384 left: still a change, the fallbacks from then.
    let mut b = Builder::new();
    b.trip("ICE 2", &[("Frankfurt", "", "08:00"), ("Hamburg", "12:04", "")]);
    b.trip("RJ 384", &[("Hamburg", "", "12:00"), ("København", "17:00", "")]);
    b.trip("S 1", &[("Hamburg", "", "12:05"), ("København", "17:20", "")]);
    let late = b.build("Frankfurt", "København");
    let net = Network::new(&late).unwrap();
    let p = plan(&late, &[("ICE 2", "Frankfurt", "Hamburg"), ("RJ 384", "Hamburg", "København")]);
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let missed = fallbacks.for_network(&crate::usable::of_chains(&late, [&p]), TimeDelta::minutes(5)).unwrap();
    assert_eq!(missed.iter().map(|c| show(&late, &c.steps)).collect::<Vec<_>>(), ["S 1 Hamburg-København"]);
    let strict = fallbacks.for_network(&crate::usable::of_chains(&late, [&p]), TimeDelta::minutes(3)).unwrap();
    assert!(strict.is_empty(), "missed by more than allowed: no change there");
}

#[test]
fn a_change_with_a_walk_gets_the_fallbacks_from_where_the_train_arrived() {
    let mut b = Builder::new();
    b.trip("STR 4", &[("Dorf", "", "08:00"), ("Zentrum", "08:30", "")]);
    b.walk("Zentrum", "Hbf", 8);
    b.trip("ICE 1", &[("Hbf", "", "09:00"), ("Ziel", "10:00", "")]);
    b.trip("ICE 3", &[("Hbf", "", "09:30"), ("Ziel", "10:30", "")]);
    let tt = b.build("Dorf", "Ziel");
    let net = Network::new(&tt).unwrap();
    let walk = tt.footpaths.iter().position(|f| tt.stations[f.from].name == "Zentrum").unwrap();
    let chain = Chain {
        steps: vec![
            ride(&tt, "STR 4", "Dorf", "Zentrum"),
            Step::Walk { footpath: walk },
            ride(&tt, "ICE 1", "Hbf", "Ziel"),
        ],
    };
    let mut fallbacks = Fallbacks::new(&net, MIN_TRANSFER, UTC);
    let all = fallbacks.for_network(&crate::usable::of_chains(&tt, [&chain]), TimeDelta::minutes(5)).unwrap();
    let shown: Vec<String> = all.iter().map(|c| show(&tt, &c.steps)).collect();
    assert!(shown.iter().any(|s| s.contains("ICE 3")), "{shown:?}");
}
