//! The user's keys for the APIs that need one. Each comes from the environment,
//! else from a `KEY=value` file in `$XDG_CONFIG_HOME/umstieg/` (or
//! `~/.config/umstieg/`): `db.env`, `trafikverket.env`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::db;

#[derive(Default)]
pub struct Keys {
    /// `DB_CLIENT_ID` and `DB_API_KEY`: DB's station boards, Germany.
    pub db: Option<db::Credentials>,
    /// `TRAFIKVERKET_KEY`: Trafikverket's train announcements, Sweden.
    pub trafikverket: Option<String>,
}

impl Keys {
    pub fn load() -> Result<Self> {
        Ok(Self::parse(&read("db.env")?, &read("trafikverket.env")?, |key| std::env::var(key).ok()))
    }

    /// Keys from the files' text, each overridden by `env(KEY)`; DB's only if
    /// both of its values are set.
    fn parse(db: &str, trafikverket: &str, env: impl Fn(&str) -> Option<String>) -> Self {
        let (db, trafikverket) = (values(db), values(trafikverket));
        let get = |file: &HashMap<String, String>, key: &str| env(key).or_else(|| file.get(key).cloned());
        Self {
            db: match (get(&db, "DB_CLIENT_ID"), get(&db, "DB_API_KEY")) {
                (Some(client_id), Some(api_key)) => Some(db::Credentials { client_id, api_key }),
                _ => None,
            },
            trafikverket: get(&trafikverket, "TRAFIKVERKET_KEY"),
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
    let Some(path) = dir().map(|d| d.join(name)) else { return Ok(String::new()) };
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// `$XDG_CONFIG_HOME/umstieg`, falling back to `~/.config/umstieg`.
fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("umstieg"))
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
