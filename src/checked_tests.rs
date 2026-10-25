use chrono::TimeDelta;

use super::*;

const MINUTE: TimeDelta = TimeDelta::minutes(1);
const MINUS_MINUTE: TimeDelta = TimeDelta::minutes(-1);

#[test]
fn sums_saturate_towards_the_overflowing_side() {
    assert_eq!(sum(MINUTE, MINUTE), TimeDelta::minutes(2));
    assert_eq!(sum(TimeDelta::MAX, MINUTE), TimeDelta::MAX);
    assert_eq!(sum(TimeDelta::MIN, MINUS_MINUTE), TimeDelta::MIN);
    assert_eq!(sum(TimeDelta::MAX, MINUS_MINUTE), TimeDelta::MAX.checked_sub(&MINUTE).unwrap(), "no overflow");
}

#[test]
fn differences_saturate_towards_the_overflowing_side() {
    assert_eq!(difference(MINUTE, MINUTE), TimeDelta::zero());
    assert_eq!(difference(TimeDelta::MIN, MINUTE), TimeDelta::MIN);
    assert_eq!(difference(TimeDelta::MAX, MINUS_MINUTE), TimeDelta::MAX);
    assert_eq!(difference(TimeDelta::MIN, MINUS_MINUTE), TimeDelta::MIN.checked_add(&MINUTE).unwrap(), "no overflow");
}
