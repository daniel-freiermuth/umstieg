//! Locks that shrug off poisoning: the guarded values are caches and job logs
//! that stay consistent between statements, so a panicking holder leaves
//! nothing a later reader would mind.

use std::sync::{Mutex, MutexGuard, PoisonError};

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
