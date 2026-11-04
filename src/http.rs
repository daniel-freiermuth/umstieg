//! The HTTP client every API module talks through.

use std::time::Duration;

/// A client sending `user_agent`, giving up on a request after `timeout`.
pub fn client(user_agent: &str, timeout: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder().user_agent(user_agent).timeout(timeout).build()
}
