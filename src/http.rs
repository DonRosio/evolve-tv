//! Client HTTPS partagé : HTTPS obligatoire, délais d'attente, taille des réponses limitée.

use std::sync::OnceLock;
use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::{Agent, Body, http::Response};

/// Taille maximale acceptée pour une réponse JSON de l'API Twitch.
const MAX_JSON: u64 = 8 * 1024 * 1024;

/// Agent HTTP unique (il garde les connexions ouvertes pour les réutiliser).
pub fn agent() -> &'static Agent {
    // `OnceLock` : créé au premier appel, puis partagé entre tous les threads.
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .https_only(true)
            // Les codes 4xx/5xx sont traités par notre code (ex. « autorisation en attente »).
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

/// Télécharge au plus `limit` octets.
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
