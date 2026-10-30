//! The user's keys for the APIs that need one. Each comes from the environment,
//! else from `keys.env` (`KEY=value` lines) in `$XDG_CONFIG_HOME/umstieg/`
//! (or `~/.config/umstieg/`).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::db;

/// The file in the config directory with every key.
const FILE: &str = "keys.env";

#[derive(Default)]
pub struct Keys {
    /// `DB_CLIENT_ID` and `DB_API_KEY`: DB's station boards, Germany.
    pub db: Option<db::Credentials>,
    /// `TRAFIKVERKET_KEY`: Trafikverket's train announcements, Sweden.
    pub trafikverket: Option<String>,
}

impl Keys {
    pub fn load() -> Result<Self> {
        Ok(Self::parse(&read(FILE)?, |key| std::env::var(key).ok()))
    }

    /// Keys from the file's text, each overridden by `env(KEY)`; DB's only if
    /// both of its values are set.
    fn parse(file: &str, env: impl Fn(&str) -> Option<String>) -> Self {
        let file = values(file);
        let get = |key: &str| env(key).or_else(|| file.get(key).cloned());
        Self {
            db: match (get("DB_CLIENT_ID"), get("DB_API_KEY")) {
                (Some(client_id), Some(api_key)) => Some(db::Credentials { client_id, api_key }),
                _ => None,
            },
            trafikverket: get("TRAFIKVERKET_KEY"),
        }
    }
}

/// `KEY=value` lines, maybe `export`ed, values maybe in double quotes.
fn values(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| line.trim().trim_start_matches("export ").split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().trim_matches('"').to_owned()))
        .collect()
}

/// The file `name` in the config directory; empty if there is none.
fn read(name: &str) -> Result<String> {
    let Some(path) = xdg_dir("XDG_CONFIG_HOME", ".config").map(|d| d.join(name)) else { return Ok(String::new()) };
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// `$<var>/umstieg`, falling back to `~/<fallback>/umstieg` when `$<var>` is
/// unset, empty or relative (the XDG Base Directory spec ignores those).
pub fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))?;
    Some(base.join("umstieg"))
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
