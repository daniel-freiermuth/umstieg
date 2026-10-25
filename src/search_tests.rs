use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Europe::Berlin;

use super::parse_time_in;

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
