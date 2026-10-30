use super::{RideRun, Usable, Wait, Walk, of_chains};
use crate::enumerate::{Chain, Step};
use crate::testkit::{Builder, at};

fn ride(trip: usize, board: usize, alight: usize) -> Step {
    Step::Ride { trip, board, alight }
}

#[test]
fn shared_rides_runs_waits_and_walks_once() {
    // ICE 1 A–B–C–D, RE 2 C–E; a walk B → B2 and S 3 B2–E.
    let mut b = Builder::new();
    let ice =
        b.trip("ICE 1", &[("A", "", "08:00"), ("B", "08:20", "08:22"), ("C", "08:40", "08:41"), ("D", "09:00", "")]);
    let re = b.trip("RE 2", &[("C", "", "08:50"), ("E", "09:30", "")]);
    let s = b.trip("S 3", &[("B2", "", "08:30"), ("E", "09:00", "")]);
    b.walk("B", "B2", 3);
    let tt = b.build("A", "E");
    let chains = [
        Chain { steps: vec![ride(ice, 0, 2), ride(re, 0, 1)] },
        Chain { steps: vec![ride(ice, 0, 1), Step::Walk { footpath: 0 }, ride(s, 0, 1)] },
        // Boarding ICE 1 again at C (as from a walk): a run of its own from there.
        Chain { steps: vec![ride(ice, 2, 3)] },
    ];
    let idx = |name: &str| tt.stations.iter().position(|st| st.name == name).unwrap();
    let used = of_chains(&tt, &chains);
    assert_eq!(
        used,
        Usable {
            rides: vec![
                // A–B–C ridden through B; C–D starts anew at C, where nobody rides through.
                RideRun { trip: ice, from: 0, to: 2 },
                RideRun { trip: ice, from: 2, to: 3 },
                RideRun { trip: re, from: 0, to: 1 },
                RideRun { trip: s, from: 0, to: 1 },
            ],
            // By station: C (index 2), then B2.
            waits: vec![
                Wait { station: idx("C"), from: at("08:40"), to: at("08:50") },
                Wait { station: idx("B2"), from: at("08:23"), to: at("08:30") },
            ],
            walks: vec![Walk { footpath: 0, start: at("08:20") }],
        }
    );
}

#[test]
fn waits_merge_per_station_and_a_missed_change_runs_back() {
    // RE 1 reaches B at 09:02; RE 2 leaves at 09:00 (missed by 2), RE 3 at 09:10, RE 4 at 09:30.
    let mut b = Builder::new();
    let r1 = b.trip("RE 1", &[("A", "", "08:00"), ("B", "09:02", "")]);
    let r2 = b.trip("RE 2", &[("B", "", "09:00"), ("C", "10:00", "")]);
    let r3 = b.trip("RE 3", &[("B", "", "09:10"), ("C", "10:10", "")]);
    let r4 = b.trip("RE 4", &[("B", "", "09:30"), ("C", "10:30", "")]);
    let tt = b.build("A", "C");
    let chains = [r2, r3, r4].map(|r| Chain { steps: vec![ride(r1, 0, 1), ride(r, 0, 1)] });
    let used = of_chains(&tt, &chains);
    assert_eq!(used.waits, [Wait { station: 1, from: at("09:00"), to: at("09:30") }]);
    // A walk first starts when the connection leaves.
    let mut b = Builder::new();
    b.walk("A", "T", 5);
    let bus = b.trip("Bus", &[("T", "", "07:00"), ("C", "10:00", "")]);
    let tt = b.build("A", "C");
    let used = of_chains(&tt, &[Chain { steps: vec![Step::Walk { footpath: 0 }, ride(bus, 0, 1)] }]);
    assert_eq!(used.walks, [Walk { footpath: 0, start: at("06:55") }]);
    assert_eq!(used.waits, []);
}
