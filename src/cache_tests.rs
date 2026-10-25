use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::anyhow;

use super::*;

const TTL: Duration = Duration::from_mins(10);

/// A fetch that counts its runs (when awaited) and answers `text`.
#[expect(clippy::unused_async, reason = "get_or_fetch takes a future; it must count only when awaited")]
async fn fetch(runs: &AtomicUsize, text: &str) -> Result<String> {
    runs.fetch_add(1, Ordering::SeqCst);
    Ok(text.to_owned())
}

/// Makes every file of the cache `age` old.
fn age_files(name: &str, age: Duration) {
    let dir = root().unwrap().join(name);
    for entry in std::fs::read_dir(dir).unwrap() {
        let file = std::fs::File::options().write(true).open(entry.unwrap().path()).unwrap();
        file.set_modified(SystemTime::now().checked_sub(age).unwrap()).unwrap();
    }
}

#[tokio::test]
async fn fresh_answers_are_reused_stale_ones_fetched_again() {
    let cache = DiskCache::new("cache-test-ttl", TTL);
    let runs = AtomicUsize::new(0);
    assert_eq!(cache.get_or_fetch("key", fetch(&runs, "first")).await.unwrap(), "first");
    assert_eq!(cache.get_or_fetch("key", fetch(&runs, "second")).await.unwrap(), "first");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "a fresh hit doesn't run the fetch");

    age_files("cache-test-ttl", Duration::from_secs(9 * 60 + 30));
    assert_eq!(cache.get_or_fetch("key", fetch(&runs, "second")).await.unwrap(), "first", "just within the TTL");

    age_files("cache-test-ttl", Duration::from_secs(10 * 60 + 1));
    assert_eq!(cache.get_or_fetch("key", fetch(&runs, "second")).await.unwrap(), "second", "stale");
    assert_eq!(cache.get_or_fetch("key", fetch(&runs, "third")).await.unwrap(), "second", "the refetch is stored");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn keys_and_caches_are_kept_apart() {
    let runs = AtomicUsize::new(0);
    let cache = DiskCache::new("cache-test-keys", TTL);
    assert_eq!(cache.get_or_fetch("http://a/plan?x", fetch(&runs, "a")).await.unwrap(), "a");
    assert_eq!(cache.get_or_fetch("http://b/plan?x", fetch(&runs, "b")).await.unwrap(), "b");
    let other = DiskCache::new("cache-test-keys-other", TTL);
    assert_eq!(other.get_or_fetch("http://a/plan?x", fetch(&runs, "c")).await.unwrap(), "c");
    assert_eq!(runs.load(Ordering::SeqCst), 3);

    // Another instance on the same directory (a later run) sees the stored answers.
    let again = DiskCache::new("cache-test-keys", TTL);
    assert_eq!(again.get_or_fetch("http://b/plan?x", fetch(&runs, "new")).await.unwrap(), "b");
    assert_eq!(runs.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn failed_fetches_are_not_cached() {
    let cache = DiskCache::new("cache-test-errors", TTL);
    let error = cache.get_or_fetch("key", async { Err(anyhow!("HTTP 503")) }).await.unwrap_err();
    assert_eq!(error.to_string(), "HTTP 503");
    let runs = AtomicUsize::new(0);
    assert_eq!(cache.get_or_fetch("key", fetch(&runs, "ok")).await.unwrap(), "ok");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn readers_never_see_half_written_files() {
    let dir = root().unwrap().join("cache-test-atomic");
    let path = dir.join("board.xml");
    let size = 1 << 20;
    let versions = *b"ab";
    let versions = versions.map(|c| String::from_utf8(vec![c; size]).unwrap());
    write_atomic(&path, &versions[0]).unwrap();
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        let reader = s.spawn(|| {
            let mut reads = 0;
            while !done.load(Ordering::SeqCst) || reads == 0 {
                let text = std::fs::read(&path).unwrap();
                assert_eq!(text.len(), size, "a partial file");
                assert!(text.iter().all(|&b| b == text[0]), "a mix of two writes");
                reads += 1;
            }
        });
        let mut writers = Vec::new();
        for text in &versions {
            writers.push(s.spawn(|| (0..20).for_each(|_| write_atomic(&path, text).unwrap())));
        }
        for writer in writers {
            writer.join().unwrap();
        }
        done.store(true, Ordering::SeqCst);
        reader.join().unwrap();
    });
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(files, ["board.xml"], "no temporary files are left");
}
#[test]
fn tests_never_use_the_users_cache() {
    let root = root().unwrap();
    assert!(root.starts_with(std::env::temp_dir()), "{}", root.display());
}
