use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use clap::Parser;

use super::{Endpoints, Report, SearchArgs, extra_time, parse_time_in};
use crate::enumerate::{self, Limits, Network};
use crate::testkit::{Builder, at};

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

#[test]
fn formats_are_read_in_the_given_zone() {
    // 2026-10-24 10:00 UTC is 12:00 in Berlin (CEST, +2).
    let now = utc(2026, 10, 24, 10, 0);
    let cases = [
        ("08:15", utc(2026, 10, 24, 6, 15)),
        // Earlier than now is still today: no rolling over to tomorrow.
        ("00:05", utc(2026, 10, 23, 22, 5)),
        ("2026-10-30 06:30", utc(2026, 10, 30, 5, 30)), // CET by then, +1
        ("2026-10-24T06:30", utc(2026, 10, 24, 4, 30)),
        ("2026-12-01", utc(2026, 11, 30, 23, 0)), // date only: midnight
    ];
    for (at, want) in cases {
        assert_eq!(parse_time_in(Some(at), &Berlin, now).unwrap(), want, "{at}");
    }
}

#[test]
fn today_is_the_date_in_the_zone_not_in_utc() {
    // 23:30 UTC on the 24th is already the 25th in Berlin.
    let now = utc(2026, 10, 24, 23, 30);
    assert_eq!(parse_time_in(Some("09:00"), &Berlin, now).unwrap(), utc(2026, 10, 25, 8, 0));
    assert_eq!(parse_time_in(Some("09:00"), &Utc, now).unwrap(), utc(2026, 10, 24, 9, 0));
}

#[test]
fn missing_or_empty_is_now() {
    let now = utc(2026, 10, 24, 10, 7);
    assert_eq!(parse_time_in(None, &Berlin, now).unwrap(), now);
    assert_eq!(parse_time_in(Some(""), &Berlin, now).unwrap(), now);
}

#[test]
fn dst_gap_is_an_error_and_the_repeated_hour_takes_the_first() {
    let now = utc(2026, 10, 24, 10, 0);
    // Clocks jump 02:00 → 03:00 on 2026-03-29: 02:30 never happens.
    let err = parse_time_in(Some("2026-03-29 02:30"), &Berlin, now).unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
    // 02:30 happens twice on 2026-10-25; the earlier one (CEST) counts.
    assert_eq!(parse_time_in(Some("2026-10-25 02:30"), &Berlin, now).unwrap(), utc(2026, 10, 25, 0, 30));
}

#[test]
fn invalid_input_is_an_error_naming_the_formats() {
    let now = utc(2026, 10, 24, 10, 0);
    for at in ["25:00", "8 Uhr", "2026-02-30 10:00", "24.10.2026 06:30", "2026-10-24 06:30:00", "tomorrow"] {
        let err = parse_time_in(Some(at), &Berlin, now).unwrap_err().to_string();
        assert!(err.contains("cannot parse") && err.contains("YYYY-MM-DD HH:MM"), "{at}: {err}");
    }
}

#[test]
fn the_user_agent_names_umstieg_its_version_and_the_contact() {
    let endpoints = Endpoints {
        efa: String::new(),
        api: String::new(),
        db_api: String::new(),
        contact: "mailto:me@example.org".to_owned(),
    };
    assert_eq!(endpoints.user_agent(), format!("umstieg/{} (mailto:me@example.org)", env!("CARGO_PKG_VERSION")));
}

#[test]
fn extra_hours_become_whole_seconds_none_if_negative() {
    assert_eq!(extra_time(1.5).unwrap(), TimeDelta::minutes(90));
    assert_eq!(extra_time(-2.0).unwrap(), TimeDelta::zero());
    // The largest accepted value is just under 10¹² seconds.
    assert_eq!(extra_time(277_777_777.0).unwrap(), TimeDelta::seconds(999_999_997_200));
}

#[test]
fn extra_hours_out_of_range_are_an_error() {
    let at_the_limit: f64 = 1e12 / 3600.0;
    assert_eq!((at_the_limit * 3600.0).to_bits(), 1e12_f64.to_bits(), "exactly 10¹² seconds");
    for hours in [at_the_limit, 3e9, -3e9, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let error = extra_time(hours).unwrap_err().to_string();
        assert!(error.starts_with("--extra-hours: ") && error.ends_with(" is out of range"), "{hours}: {error}");
    }
}

/// The search options of `argv` (after the program name).
fn search_args(argv: &[&str]) -> SearchArgs {
    #[derive(clap::Parser)]
    struct Line {
        #[command(flatten)]
        args: SearchArgs,
    }
    Line::try_parse_from(std::iter::once("umstieg").chain(argv.iter().copied())).unwrap().args
}

/// The report for `tt`, departing 08:00–09:00, changes taking no time at all.
fn report(tt: crate::timetable::Timetable) -> String {
    let args = search_args(&["A", "C"]);
    let limits = Limits {
        origin: tt.origin,
        depart_from: at("08:00"),
        depart_until: at("09:00"),
        max_changes: args.max_changes,
        extra_time: TimeDelta::hours(1),
        min_transfer: TimeDelta::zero(),
        missed_by: TimeDelta::minutes(5),
        keep_detours: false,
        forward_only: false,
        max_nodes: args.max_nodes,
    };
    let enumeration = enumerate::enumerate(&Network::new(&tt).unwrap(), &limits).unwrap();
    let report = Report {
        args,
        built: crate::timetable::Build { timetable: tt, discovered: Vec::new(), warnings: Vec::new() },
        enumeration,
        limits,
        hubs: Vec::new(),
        requests: "0 Transitous requests".to_owned(),
        with_db: false,
        time: at("08:00"),
        depart_until: at("09:00"),
        min_transfer: TimeDelta::zero(),
        plot: None,
    };
    let (stages, _) = tokio::sync::mpsc::unbounded_channel();
    report.write(&stages).unwrap().text
}

#[test]
fn a_change_without_slack_is_made_not_missed() {
    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    b.trip("RE 2", &[("B", "", "09:00"), ("C", "10:00", "")]);
    let text = report(b.build("A", "C"));
    let summary = text.lines().find(|l| l.starts_with("fastest ")).unwrap();
    assert!(summary.contains(" · 1 connections in 1 groups ("), "{text}");
    assert!(!text.contains("missed"), "{text}");

    let mut b = Builder::new();
    b.trip("RE 1", &[("A", "", "08:00"), ("B", "09:00+1", "")]);
    b.trip("RE 2", &[("B", "", "09:00"), ("C", "10:00", "")]);
    let text = report(b.build("A", "C"));
    let summary = text.lines().find(|l| l.starts_with("fastest ")).unwrap();
    assert!(summary.contains(" · 1 connections in 1 groups, 1 with a change missed by ≤ 5m ("), "{text}");
}
