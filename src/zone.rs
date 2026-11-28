//! Time zones and the wire format of times. Zones come from the system's
//! tzdb (/usr/share/zoneinfo), so a tzdata update applies without a rebuild;
//! jiff's bundled copy stands in where the system has none.

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::{AmbiguousOffset, TimeZone};

/// German local time: DB's boards and EFA's requests are in it. Looked up
/// on every call (jiff caches it, revalidating against the tzdb's files), so
/// a running server follows a tzdata update here as it does for the user's zone.
pub fn berlin() -> TimeZone {
    TimeZone::get("Europe/Berlin").unwrap_or_else(|_| jiff::tz::get!("Europe/Berlin"))
}

/// Swedish local time: SJ's dates are in it. Looked up like [`berlin`].
pub fn stockholm() -> TimeZone {
    TimeZone::get("Europe/Stockholm").unwrap_or_else(|_| jiff::tz::get!("Europe/Stockholm"))
}

/// The first moment the wall time `dt` stands for in `zone`: in a repeated
/// hour the earlier one; `None` in a skipped hour.
pub fn earliest(zone: &TimeZone, dt: DateTime) -> Option<Timestamp> {
    let ambiguous = zone.to_ambiguous_timestamp(dt);
    match ambiguous.offset() {
        AmbiguousOffset::Gap { .. } => None,
        AmbiguousOffset::Unambiguous { .. } | AmbiguousOffset::Fold { .. } => ambiguous.earlier().ok(),
    }
}

/// `t` in UTC to the second, as APIs take it: "2026-10-24T06:30:00Z".
pub fn utc_seconds(t: Timestamp) -> String {
    t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}
