//! Short-lived on-disk cache for routing answers: repeated or tweaked searches
//! don't hit volunteer-run or public servers again, while live data stays fresh.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};

pub struct DiskCache {
    dir: Option<PathBuf>,
    ttl: Duration,
}

/// `$XDG_CACHE_HOME/umstieg` (or `~/.cache/umstieg`); under tests a temporary
/// directory, so tests never read or write the user's caches.
#[cfg_attr(test, expect(clippy::unnecessary_wraps, reason = "only the user's cache root can be missing"))]
pub fn root() -> Option<PathBuf> {
    #[cfg(test)]
    {
        static ROOT: std::sync::LazyLock<PathBuf> =
            std::sync::LazyLock::new(|| std::env::temp_dir().join(format!("umstieg-test-{}", std::process::id())));
        Some(ROOT.clone())
    }
    #[cfg(not(test))]
    crate::keys::xdg_dir("XDG_CACHE_HOME", ".cache")
}

/// Writes `contents` to `path` (creating its directory) through a temporary
/// file and a rename, so concurrent searches read the old or the new file,
/// never a half-written one.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    static TEMPORARIES: AtomicUsize = AtomicUsize::new(0);
    let dir = path.parent().with_context(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let temporary =
        dir.join(format!(".{name}.{}.{}.tmp", std::process::id(), TEMPORARIES.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&temporary, contents).with_context(|| format!("writing {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("renaming to {}", path.display())).inspect_err(|_| {
        std::fs::remove_file(&temporary).ok();
    })
}

impl DiskCache {
    /// `<cache root>/<name>`.
    pub fn new(name: &str, ttl: Duration) -> Self {
        Self { dir: root().map(|d| d.join(name)), ttl }
    }

    /// The cached text for `key` if fresh, else `fetch()`'s result, stored if
    /// the cache is writable (a failing cache must not fail the request).
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
            tracing::debug!(key, "cache hit");
            return Ok(text);
        }
        tracing::debug!(key, "cache miss");
        let text = fetch.await?;
        if let Some(file) = &file
            && let Err(e) = write_atomic(file, &text)
        {
            tracing::warn!(key, "not cached: {e:#}");
        }
        Ok(text)
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
