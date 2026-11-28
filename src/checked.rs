//! Time arithmetic that can't panic. jiff's operators panic on overflow; feed
//! data and command-line options are untrusted, so sums either fail the
//! search with an error or, for internal bounds where "later than anything"
//! is the right answer, saturate. Durations saturate with
//! `SignedDuration::saturating_add` and `saturating_sub`.

use anyhow::{Context, Result};
use jiff::{SignedDuration, Timestamp};

/// `t + d`, or an error naming what overflowed.
pub fn later(t: Timestamp, d: SignedDuration, what: &str) -> Result<Timestamp> {
    t.checked_add(d).with_context(|| format!("{what}: {t} + {d} is out of range"))
}

/// `t + d`, at most the latest representable time: for bounds like deadlines.
pub fn later_or_max(t: Timestamp, d: SignedDuration) -> Timestamp {
    t.checked_add(d).unwrap_or(Timestamp::MAX)
}

/// `t - d`, at least the earliest representable time.
pub fn earlier_or_min(t: Timestamp, d: SignedDuration) -> Timestamp {
    t.checked_sub(d).unwrap_or(Timestamp::MIN)
}

/// `minutes` as a duration, or an error naming the option.
pub fn minutes(minutes: i64, what: &str) -> Result<SignedDuration> {
    SignedDuration::try_from_mins(minutes).with_context(|| format!("{what}: {minutes} minutes is out of range"))
}
