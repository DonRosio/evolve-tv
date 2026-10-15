//! Shared HTTPS client: HTTPS mandatory, timeouts, response size limited.

use std::sync::OnceLock;
use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::{Agent, Body, http::Response};

/// Maximum size accepted for a JSON response from the Twitch API.
const MAX_JSON: u64 = 8 * 1024 * 1024;

/// Single HTTP agent (it keeps connections open to reuse them).
pub fn agent() -> &'static Agent {
    // `OnceLock`: created at the first call, then shared between all the threads.
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .https_only(true)
            // 4xx/5xx codes are handled by our code (e.g. "authorization pending").
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .max_redirects(3)
            .user_agent(concat!("evolve-tv/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

pub fn read_json<T: DeserializeOwned>(response: &mut Response<Body>) -> Result<T, String> {
    response
        .body_mut()
        .with_config()
        .limit(MAX_JSON)
        .read_json()
        .map_err(|e| format!("réponse illisible : {e}"))
}

/// Downloads at most `limit` bytes.
pub fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut response = agent().get(url).call().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| e.to_string())
}
