use std::collections::HashSet;

use chrono::{Local, TimeDelta, TimeZone, Utc};

use super::{buffer, clock, duration, header, route, timed};
use crate::enumerate::{Chain, Step};
use crate::testkit::{Builder, time};
use crate::timetable::Timetable;

const MIN: TimeDelta = TimeDelta::minutes(5);

fn ride(trip: usize, board: usize, alight: usize) -> Step {
    Step::Ride { trip, board, alight }
}

/// Every station in UTC, so clocks don't depend on the machine's zone.
fn in_utc(mut tt: Timetable) -> Timetable {
    for s in &mut tt.stations {
        s.tz = Some("UTC".to_owned());
    }
    tt
}

#[test]
fn clock_uses_the_station_zone_with_dst() {
    let utc = |m, d, h, mi| Utc.with_ymd_and_hms(2026, m, d, h, mi, 0).unwrap();
    let cases = [
        (utc(10, 24, 4, 39), "Europe/Berlin", "06:39"), // CEST
        (utc(10, 26, 4, 39), "Europe/Berlin", "05:39"), // CET
        (utc(10, 25, 0, 30), "Europe/Berlin", "02:30"), // first 02:30 of the night
        (utc(10, 25, 1, 30), "Europe/Berlin", "02:30"), // and the repeated one
        (utc(3, 29, 1, 0), "Europe/Berlin", "03:00"),   // 02:00 skipped
        (utc(10, 24, 4, 39), "Europe/London", "05:39"),
        (utc(10, 24, 4, 39), "America/New_York", "00:39"),
        (utc(10, 24, 23, 59), "UTC", "23:59"),
    ];
    for (t, tz, want) in cases {
        assert_eq!(clock(t, Some(tz)), want, "{t} in {tz}");
    }
}

#[test]
fn clock_falls_back_to_local_time_for_unknown_or_missing_zones() {
    let t = Utc.with_ymd_and_hms(2026, 10, 24, 4, 39, 0).unwrap();
    let local = t.with_timezone(&Local).format("%H:%M").to_string();
    for tz in [None, Some(""), Some("Mars/Olympus_Mons"), Some("europe/berlin ")] {
        assert_eq!(clock(t, tz), local, "{tz:?}");
    }
}

#[test]
fn duration_switches_to_hours_at_sixty_minutes_and_keeps_the_sign() {
    let cases = [
        (0, "0m"),
        (59, "59m"),
        (60, "1h00"),
        (61, "1h01"),
        (663, "11h03"),
        (1500, "25h00"),
        (-5, "-5m"),
        (-60, "-1h00"),
        (-90, "-1h30"),
    ];
    for (minutes, want) in cases {
        assert_eq!(duration(TimeDelta::minutes(minutes)), want, "{minutes}");
    }
    // Whole minutes only, rounded towards zero.
    assert_eq!(duration(TimeDelta::seconds(59 * 60 + 59)), "59m");
    assert_eq!(duration(TimeDelta::seconds(-59)), "0m");
}

#[test]
fn timed_shows_the_actual_time_and_the_delay_sign() {
    let cases = [("", "--:--"), ("09:02", "09:02"), ("09:02+5", "09:07 (+5)"), ("09:02-2", "09:00 (-2)")];
    for (t, want) in cases {
        assert_eq!(timed(time(t), Some("UTC")), want, "{t}");
    }
    assert_eq!(timed(time("23:58+5"), Some("Europe/Berlin")), "02:03 (+5)");
}

#[test]
fn buffer_warns_below_the_minimum_and_marks_missed_changes() {
    let cases = [
        (12, "12m"),
        (5, "5m"), // exactly the minimum is fine
        (4, "4m⚠"),
        (0, "0m⚠"),
        (75, "1h15"),
        (-2, "missed 2m✗"),
        (-75, "missed 1h15✗"),
    ];
    for (slack, want) in cases {
        assert_eq!(buffer(TimeDelta::minutes(slack), MIN), want, "{slack}");
    }
    assert_eq!(buffer(TimeDelta::zero(), TimeDelta::zero()), "0m");
}

#[test]
fn header_shows_times_in_origin_and_destination_zones_and_the_smallest_buffer() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "06:39"), ("B", "08:00+3", "")]);
    let re = b.trip("RE 2", &[("B", "", "08:10"), ("C", "09:00", "")]);
    let s = b.trip("S 3", &[("C", "", "09:04"), ("D", "21:05+1", "")]);
    let mut tt = in_utc(b.build("A", "D"));
    tt.stations[tt.destination].tz = Some("Europe/London".to_owned());

    let two = Chain { steps: vec![ride(ice, 0, 1), ride(re, 0, 1), ride(s, 0, 1)] };
    // Buffers 7m and 4m: the smaller, flagged. Arrival 21:06 UTC is 22:06 in London.
    assert_eq!(header(&tt, &two, MIN), "06:39 → 22:06  14h27  2×  min 4m⚠");

    tt.destination = tt.stations.iter().position(|s| s.name == "B").unwrap();
    let direct = Chain { steps: vec![ride(ice, 0, 1)] };
    assert_eq!(header(&tt, &direct, MIN), "06:39 → 08:03  1h24  0×  direct");

    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00+8", "")]);
    let re = b.trip("RE 2", &[("B", "", "09:05"), ("C", "10:00", "")]);
    let tt = in_utc(b.build("A", "C"));
    let missed = Chain { steps: vec![ride(ice, 0, 1), ride(re, 0, 1)] };
    assert_eq!(header(&tt, &missed, MIN), "08:00 → 10:00  2h00  1×  min missed 3m✗");
}

#[test]
fn header_arrival_includes_a_final_walk() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    b.walk("B", "C", 10);
    let tt = in_utc(b.build("A", "C"));
    let chain = Chain { steps: vec![ride(ice, 0, 1), Step::Walk { footpath: 0 }] };
    assert_eq!(header(&tt, &chain, MIN), "08:00 → 09:10  1h10  0×  direct");
}

#[test]
fn route_lists_trains_changes_and_buffers() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00+2", "09:02+2"), ("C", "10:00+2", "")]);
    let re = b.trip("RE 2", &[("C", "", "10:05"), ("D", "10:40", "")]);
    let s = b.trip("S 3", &[("D2", "", "10:50"), ("E", "11:00", "")]);
    b.walk("D", "D2", 4);
    let tt = in_utc(b.build("A", "E"));
    let none = HashSet::new();

    let direct = Chain { steps: vec![ride(ice, 0, 2)] };
    assert_eq!(route(&tt, &direct, MIN, &none), "08:00 ICE 1 → 10:02 (+2) C");

    let chain = Chain { steps: vec![ride(ice, 0, 2), ride(re, 0, 1), Step::Walk { footpath: 0 }, ride(s, 0, 1)] };
    assert_eq!(
        route(&tt, &chain, MIN, &none),
        // Same station: not repeated; another station after a walk: named.
        "08:00 ICE 1 → 10:02 (+2) C ‖3m⚠‖ 10:05 RE 2 → 10:40 D ‖6m walk 4m‖ 10:50 D2 S 3 → 11:00 E"
    );
}

#[test]
fn route_names_the_change_station_only_when_its_name_differs() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    let re = b.trip("RE 2", &[("B'", "", "09:10"), ("C", "10:00", "")]);
    b.walk("B", "B'", 2);
    let mut tt = in_utc(b.build("A", "C"));
    let chain = Chain { steps: vec![ride(ice, 0, 1), Step::Walk { footpath: 0 }, ride(re, 0, 1)] };
    assert_eq!(route(&tt, &chain, MIN, &HashSet::new()), "08:00 ICE 1 → 09:00 B ‖8m walk 2m‖ 09:10 B' RE 2 → 10:00 C");
    // Two feeds' entries of one station: same name, nothing to tell apart.
    let b2 = tt.stations.iter().position(|s| s.name == "B'").unwrap();
    tt.stations[b2].name = "B".to_owned();
    assert_eq!(route(&tt, &chain, MIN, &HashSet::new()), "08:00 ICE 1 → 09:00 B ‖8m walk 2m‖ 09:10 RE 2 → 10:00 C");
}

#[test]
fn route_stops_at_the_first_train_of_the_plan() {
    let mut b = Builder::new();
    let s1 = b.trip("S1", &[("B", "", "12:42"), ("Altona", "12:54", "")]);
    let rj = b.trip("RJ 384", &[("Altona", "", "13:08"), ("Flensburg", "15:00", "15:05"), ("K", "18:00", "")]);
    let ic = b.trip("IC 9", &[("Flensburg", "", "15:20"), ("K", "18:30", "")]);
    let tt = in_utc(b.build("B", "K"));
    let chain = Chain { steps: vec![ride(s1, 0, 1), ride(rj, 0, 1), ride(ic, 0, 1)] };

    let rejoin = HashSet::from([rj]);
    assert_eq!(route(&tt, &chain, MIN, &rejoin), "12:42 S1 → 12:54 Altona ‖14m‖ 13:08 RJ 384 (back on plan)");
    // The first rejoining train counts, even if a later one is in the plan too.
    let rejoin = HashSet::from([rj, ic]);
    assert_eq!(route(&tt, &chain, MIN, &rejoin), "12:42 S1 → 12:54 Altona ‖14m‖ 13:08 RJ 384 (back on plan)");
    // Back on plan from the first ride on.
    let rejoin = HashSet::from([s1]);
    assert_eq!(route(&tt, &chain, MIN, &rejoin), "12:42 S1 (back on plan)");
}

#[test]
fn route_ends_with_a_walk_to_the_destination() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    b.walk("B", "C", 10);
    let tt = in_utc(b.build("A", "C"));
    let chain = Chain { steps: vec![ride(ice, 0, 1), Step::Walk { footpath: 0 }] };
    assert_eq!(route(&tt, &chain, MIN, &HashSet::new()), "08:00 ICE 1 → 09:00 B walk 10m to C");
}
