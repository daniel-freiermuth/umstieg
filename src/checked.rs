//! Time arithmetic that can't panic. chrono's operators panic on overflow;
//! feed data and command-line options are untrusted, so sums either fail the
//! search with an error or, for internal bounds where "later than anything"
//! is the right answer, saturate.

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};

/// `t + d`, or an error naming what overflowed.
pub fn later(t: DateTime<Utc>, d: TimeDelta, what: &str) -> Result<DateTime<Utc>> {
    t.checked_add_signed(d).with_context(|| format!("{what}: {t} + {d} is out of range"))
}

/// `t + d`, at most the latest representable time: for bounds like deadlines.
pub fn later_or_max(t: DateTime<Utc>, d: TimeDelta) -> DateTime<Utc> {
    t.checked_add_signed(d).unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// `t - d`, at least the earliest representable time.
pub fn earlier_or_min(t: DateTime<Utc>, d: TimeDelta) -> DateTime<Utc> {
    t.checked_sub_signed(d).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// `a + b` for durations, saturating at the extremes.
pub fn sum(a: TimeDelta, b: TimeDelta) -> TimeDelta {
    a.checked_add(&b).unwrap_or_else(|| if b < TimeDelta::zero() { TimeDelta::MIN } else { TimeDelta::MAX })
}

/// `a - b` for durations, saturating at the extremes.
pub fn difference(a: TimeDelta, b: TimeDelta) -> TimeDelta {
    a.checked_sub(&b).unwrap_or_else(|| if b > TimeDelta::zero() { TimeDelta::MIN } else { TimeDelta::MAX })
}

/// `minutes` as a duration, or an error naming the option.
pub fn minutes(minutes: i64, what: &str) -> Result<TimeDelta> {
    TimeDelta::try_minutes(minutes).with_context(|| format!("{what}: {minutes} minutes is out of range"))
}

#[cfg(test)]
#[path = "checked_tests.rs"]
mod tests;
