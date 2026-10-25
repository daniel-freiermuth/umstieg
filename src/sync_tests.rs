use std::sync::Mutex;

use super::lock;

#[test]
#[expect(clippy::significant_drop_tightening, reason = "only panicking while holding the lock poisons it")]
fn a_poisoned_lock_still_yields_the_value() {
    let mutex = Mutex::new(vec![1]);
    let panicked = std::thread::scope(|s| {
        s.spawn(|| {
            let mut guard = mutex.lock().unwrap();
            guard.push(2);
            panic!("holder panics");
        })
        .join()
        .is_err()
    });
    assert!(panicked && mutex.is_poisoned());
    lock(&mutex).push(3);
    assert_eq!(*lock(&mutex), [1, 2, 3]);
}
