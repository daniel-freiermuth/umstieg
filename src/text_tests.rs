use std::collections::HashSet;

use chrono::{TimeDelta, TimeZone, Utc};
use chrono_tz::America::New_York;
use chrono_tz::Europe::{Berlin, London};
use chrono_tz::UTC;

use super::{buffer, clock, duration, header, route, timed, window};
use crate::enumerate::{Chain, Step};
use crate::testkit::{Builder, time};

const MIN: TimeDelta = TimeDelta::minutes(5);

fn ride(trip: usize, board: usize, alight: usize) -> Step {
    Step::Ride { trip, board, alight }
}

#[test]
fn clock_is_in_the_zone_with_its_abbreviation_and_dst() {
    let utc = |m, d, h, mi| Utc.with_ymd_and_hms(2026, m, d, h, mi, 0).unwrap();
    let cases = [
        (utc(10, 24, 4, 39), Berlin, "06:39 CEST"),
        (utc(10, 26, 4, 39), Berlin, "05:39 CET"),
        (utc(10, 25, 0, 30), Berlin, "02:30 CEST"), // first 02:30 of the night
        (utc(10, 25, 1, 30), Berlin, "02:30 CET"),  // and the repeated one, told apart
        (utc(3, 29, 1, 0), Berlin, "03:00 CEST"),   // 02:00 skipped
        (utc(10, 24, 4, 39), London, "05:39 BST"),
        (utc(10, 24, 4, 39), New_York, "00:39 EDT"),
        (utc(10, 24, 23, 59), UTC, "23:59 UTC"),
    ];
    for (t, zone, want) in cases {
        assert_eq!(clock(t, zone), want, "{t} in {zone}");
    }
}

#[test]
fn window_has_the_date_in_the_zone_and_the_end_date_only_when_it_differs() {
    let utc = |m, d, h, mi| Utc.with_ymd_and_hms(2026, m, d, h, mi, 0).unwrap();
    let cases = [
        (utc(10, 29, 5, 0), utc(10, 29, 7, 0), Berlin, "Thu 29 Oct 2026, 06:00 CET–08:00 CET"),
        // 23:30 UTC is already the next day in Berlin.
        (utc(10, 29, 23, 30), utc(10, 30, 1, 0), Berlin, "Fri 30 Oct 2026, 00:30 CET–02:00 CET"),
        (utc(10, 29, 22, 0), utc(10, 29, 23, 30), Berlin, "Thu 29 Oct 2026, 23:00 CET – Fri 30 Oct, 00:30 CET"),
        (utc(10, 25, 0, 30), utc(10, 25, 1, 30), Berlin, "Sun 25 Oct 2026, 02:30 CEST–02:30 CET"),
        (utc(10, 29, 5, 0), utc(10, 29, 7, 0), New_York, "Thu 29 Oct 2026, 01:00 EDT–03:00 EDT"),
    ];
    for (from, until, zone, want) in cases {
        assert_eq!(window(from, until, zone), want, "{from}–{until} in {zone}");
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
    let cases = [("", "--:--"), ("09:02", "09:02 UTC"), ("09:02+5", "09:07 UTC (+5)"), ("09:02-2", "09:00 UTC (-2)")];
    for (t, want) in cases {
        assert_eq!(timed(time(t), UTC), want, "{t}");
    }
    assert_eq!(timed(time("23:58+5"), Berlin), "02:03 CEST (+5)");
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
fn header_shows_times_in_the_given_zone_whatever_the_stations_and_the_smallest_buffer() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "06:39"), ("B", "08:00+3", "")]);
    let re = b.trip("RE 2", &[("B", "", "08:10"), ("C", "09:00", "")]);
    let s = b.trip("S 3", &[("C", "", "09:04"), ("D", "21:05+1", "")]);
    let mut tt = b.build("A", "D");
    tt.stations[tt.destination].tz = Some("Europe/London".to_owned());

    let two = Chain { steps: vec![ride(ice, 0, 1), ride(re, 0, 1), ride(s, 0, 1)] };
    // Buffers 7m and 4m: the smaller, flagged. 06:39 and 21:06 UTC are 08:39 and 23:06 in Berlin.
    assert_eq!(header(&tt, &two, MIN, Berlin).unwrap(), "08:39 CEST → 23:06 CEST  14h27  2×  min 4m⚠");

    tt.destination = tt.stations.iter().position(|s| s.name == "B").unwrap();
    let direct = Chain { steps: vec![ride(ice, 0, 1)] };
    assert_eq!(header(&tt, &direct, MIN, UTC).unwrap(), "06:39 UTC → 08:03 UTC  1h24  0×  direct");

    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00+8", "")]);
    let re = b.trip("RE 2", &[("B", "", "09:05"), ("C", "10:00", "")]);
    let tt = b.build("A", "C");
    let missed = Chain { steps: vec![ride(ice, 0, 1), ride(re, 0, 1)] };
    assert_eq!(header(&tt, &missed, MIN, UTC).unwrap(), "08:00 UTC → 10:00 UTC  2h00  1×  min missed 3m✗");
}

#[test]
fn header_arrival_includes_a_final_walk() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    b.walk("B", "C", 10);
    let tt = b.build("A", "C");
    let chain = Chain { steps: vec![ride(ice, 0, 1), Step::Walk { footpath: 0 }] };
    assert_eq!(header(&tt, &chain, MIN, UTC).unwrap(), "08:00 UTC → 09:10 UTC  1h10  0×  direct");
}

#[test]
fn route_lists_trains_changes_and_buffers() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00+2", "09:02+2"), ("C", "10:00+2", "")]);
    let re = b.trip("RE 2", &[("C", "", "10:05"), ("D", "10:40", "")]);
    let s = b.trip("S 3", &[("D2", "", "10:50"), ("E", "11:00", "")]);
    b.walk("D", "D2", 4);
    let tt = b.build("A", "E");
    let none = HashSet::new();

    let direct = Chain { steps: vec![ride(ice, 0, 2)] };
    assert_eq!(route(&tt, &direct, MIN, &none, UTC).unwrap(), "08:00 UTC ICE 1 → 10:02 UTC (+2) C");

    let chain = Chain { steps: vec![ride(ice, 0, 2), ride(re, 0, 1), Step::Walk { footpath: 0 }, ride(s, 0, 1)] };
    assert_eq!(
        route(&tt, &chain, MIN, &none, UTC).unwrap(),
        // Same station: not repeated; another station after a walk: named.
        "08:00 UTC ICE 1 → 10:02 UTC (+2) C ‖3m⚠‖ 10:05 UTC RE 2 → 10:40 UTC D ‖6m walk 4m‖ \
         10:50 UTC D2 S 3 → 11:00 UTC E"
    );
}

#[test]
fn route_names_the_change_station_only_when_its_name_differs() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    let re = b.trip("RE 2", &[("B'", "", "09:10"), ("C", "10:00", "")]);
    b.walk("B", "B'", 2);
    let mut tt = b.build("A", "C");
    let chain = Chain { steps: vec![ride(ice, 0, 1), Step::Walk { footpath: 0 }, ride(re, 0, 1)] };
    assert_eq!(
        route(&tt, &chain, MIN, &HashSet::new(), UTC).unwrap(),
        "08:00 UTC ICE 1 → 09:00 UTC B ‖8m walk 2m‖ 09:10 UTC B' RE 2 → 10:00 UTC C"
    );
    // Two feeds' entries of one station: same name, nothing to tell apart.
    let b2 = tt.stations.iter().position(|s| s.name == "B'").unwrap();
    tt.stations[b2].name = "B".to_owned();
    assert_eq!(
        route(&tt, &chain, MIN, &HashSet::new(), UTC).unwrap(),
        "08:00 UTC ICE 1 → 09:00 UTC B ‖8m walk 2m‖ 09:10 UTC RE 2 → 10:00 UTC C"
    );
}

#[test]
fn route_stops_at_the_first_train_of_the_plan() {
    let mut b = Builder::new();
    let s1 = b.trip("S1", &[("B", "", "12:42"), ("Altona", "12:54", "")]);
    let rj = b.trip("RJ 384", &[("Altona", "", "13:08"), ("Flensburg", "15:00", "15:05"), ("K", "18:00", "")]);
    let ic = b.trip("IC 9", &[("Flensburg", "", "15:20"), ("K", "18:30", "")]);
    let tt = b.build("B", "K");
    let chain = Chain { steps: vec![ride(s1, 0, 1), ride(rj, 0, 1), ride(ic, 0, 1)] };

    let want = "12:42 UTC S1 → 12:54 UTC Altona ‖14m‖ 13:08 UTC RJ 384 (back on plan)";
    let rejoin = HashSet::from([rj]);
    assert_eq!(route(&tt, &chain, MIN, &rejoin, UTC).unwrap(), want);
    // The first rejoining train counts, even if a later one is in the plan too.
    let rejoin = HashSet::from([rj, ic]);
    assert_eq!(route(&tt, &chain, MIN, &rejoin, UTC).unwrap(), want);
    // Back on plan from the first ride on.
    let rejoin = HashSet::from([s1]);
    assert_eq!(route(&tt, &chain, MIN, &rejoin, UTC).unwrap(), "12:42 UTC S1 (back on plan)");
}

#[test]
fn route_ends_with_a_walk_to_the_destination() {
    let mut b = Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    b.walk("B", "C", 10);
    let tt = b.build("A", "C");
    let chain = Chain { steps: vec![ride(ice, 0, 1), Step::Walk { footpath: 0 }] };
    assert_eq!(route(&tt, &chain, MIN, &HashSet::new(), UTC).unwrap(), "08:00 UTC ICE 1 → 09:00 UTC B walk 10m to C");
}
