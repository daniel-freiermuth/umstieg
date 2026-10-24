//! Short-lived on-disk cache for routing answers: repeated or tweaked searches
//! don't hit volunteer-run or public servers again, while live data stays fresh.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

pub struct DiskCache {
    dir: Option<PathBuf>,
    ttl: Duration,
}

impl DiskCache {
    /// `$XDG_CACHE_HOME/umstieg/<name>` (or `~/.cache/…`).
    pub fn new(name: &str, ttl: Duration) -> Self {
        let dir = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|d| d.join("umstieg").join(name));
        Self { dir, ttl }
    }

    /// The cached text for `key` if fresh, else `fetch()`'s result, stored.
    pub async fn get_or_fetch<F>(&self, key: &str, fetch: F) -> Result<String>
    where
        F: Future<Output = Result<String>>,
    {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        let file = self.dir.as_ref().map(|d| d.join(format!("{:016x}.json", hasher.finish())));
        let fresh = file.as_ref().and_then(|f| {
            let age = std::fs::metadata(f).ok()?.modified().ok()?.elapsed().ok()?;
            (age < self.ttl).then(|| std::fs::read_to_string(f).ok()).flatten()
        });
        if let Some(text) = fresh {
            return Ok(text);
        }
        let text = fetch.await?;
        if let Some(file) = &file {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            }
            std::fs::write(file, &text).with_context(|| format!("writing {}", file.display()))?;
        }
        Ok(text)
    }
}
