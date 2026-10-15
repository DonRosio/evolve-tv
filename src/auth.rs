//! Twitch account login with the "Device Code Flow" and storage of the token in
//! the GNOME keyring (Secret Service).
//!
//! How it goes: the application asks Twitch for a code, you validate it on
//! twitch.tv/activate in your browser, and the application receives an access
//! token (valid ~4 h) and a refresh token. No password or
//! secret goes through the application.

use std::sync::OnceLock;

use keyring_core::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};

use crate::http;

/// Requested rights: read and write in the chat, know your emotes.
pub const SCOPES: &str = "chat:read chat:edit user:read:emotes";
const REQUIRED_SCOPES: [&str; 2] = ["chat:read", "chat:edit"];

const KEYRING_SERVICE: &str = "evolve-tv";
const KEYRING_ACCOUNT: &str = "twitch";

/// OAuth tokens. Deliberately without `Debug`: they cannot be printed by mistake.
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
}

/// Identity of the account, returned by Twitch when the token is checked.
#[derive(Clone, Debug, Deserialize)]
pub struct Identity {
    pub login: String,
    pub user_id: String,
    pub client_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// Logged-in session: tokens + identity.
#[derive(Clone)]
pub struct Session {
    pub tokens: Tokens,
    pub identity: Identity,
}

pub enum Outcome {
    LoggedIn(Session),
    /// No token, or a revoked token: one must log in (again).
    LoggedOut,
    /// Temporary error (network…): the tokens are kept, we will try again.
    Failed(String),
}

#[derive(Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

pub enum Poll {
    Pending,
    SlowDown,
    Done(Tokens),
    Expired,
}

/// Error response from id.twitch.tv: `{"status":400,"message":"authorization_pending"}`.
#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

// ---------- Trousseau ----------

fn keyring_entry() -> Result<Entry, String> {
    // The keyring is initialised only once, at first use.
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let store = zbus_secret_service_keyring_store::Store::new()
            .map_err(|e| format!("trousseau inaccessible : {e}"))?;
        keyring_core::set_default_store(store);
        Ok(())
    })
    .clone()?;
    Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|e| format!("trousseau : {e}"))
}

pub fn load_tokens() -> Result<Option<Tokens>, String> {
    match keyring_entry()?.get_password() {
        Ok(json) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|_| "jeton du trousseau illisible".into()),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(e) => Err(format!("trousseau : {e}")),
    }
}

pub fn save_tokens(tokens: &Tokens) -> Result<(), String> {
    let json = serde_json::to_string(tokens).map_err(|e| e.to_string())?;
    keyring_entry()?
        .set_password(&json)
        .map_err(|e| format!("trousseau : {e}"))
}

pub fn delete_tokens() -> Result<(), String> {
    match keyring_entry()?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(e) => Err(format!("trousseau : {e}")),
    }
}

// ---------- API OAuth de Twitch ----------

/// Step 1: asks for a code to show to the user.
pub fn start_device_flow(client_id: &str) -> Result<DeviceCode, String> {
    let mut response = http::agent()
        .post("https://id.twitch.tv/oauth2/device")
        .send_form([("client_id", client_id), ("scopes", SCOPES)])
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        let error: ApiError = http::read_json(&mut response).unwrap_or(ApiError {
            message: String::new(),
        });
        return Err(format!(
            "Twitch refuse la demande ({}) : {}",
            response.status(),
            error.message
        ));
    }
    let code: DeviceCode = http::read_json(&mut response)?;
    // Never opens in the browser an address that is not Twitch's.
    if !code
        .verification_uri
        .starts_with("https://www.twitch.tv/activate")
    {
        return Err("adresse de validation inattendue".into());
    }
    Ok(code)
}

/// Step 2: asks (every `interval` seconds) whether the user has validated the code.
pub fn poll_device_flow(client_id: &str, device_code: &str) -> Result<Poll, String> {
    let mut response = http::agent()
        .post("https://id.twitch.tv/oauth2/token")
        .send_form([
            ("client_id", client_id),
            ("scopes", SCOPES),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .map_err(|e| e.to_string())?;
    if response.status().is_success() {
        return http::read_json(&mut response).map(Poll::Done);
    }
    let error: ApiError = http::read_json(&mut response)?;
    match error.message.as_str() {
        "authorization_pending" => Ok(Poll::Pending),
        "slow_down" => Ok(Poll::SlowDown),
        "invalid device code" | "expired_token" | "access_denied" => Ok(Poll::Expired),
        other => Err(format!("Twitch : {other}")),
    }
}

/// Result of a token renewal.
enum Refresh {
    Ok(Tokens),
    /// Refresh token refused: one must log in again.
    Rejected,
}

fn refresh(client_id: &str, refresh_token: &str) -> Result<Refresh, String> {
    // "Public" client: no secret, only the application's identifier.
    let mut response = http::agent()
        .post("https://id.twitch.tv/oauth2/token")
        .send_form([
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .map_err(|e| e.to_string())?;
    match response.status().as_u16() {
        200 => http::read_json(&mut response).map(Refresh::Ok),
        400 | 401 => Ok(Refresh::Rejected),
        status => Err(format!("renouvellement du jeton : HTTP {status}")),
    }
}

/// Checks the token with Twitch (mandatory at start-up, then every hour).
/// `Ok(None)`: expired or revoked token.
fn validate(access_token: &str) -> Result<Option<Identity>, String> {
    let mut response = http::agent()
        .get("https://id.twitch.tv/oauth2/validate")
        .header("Authorization", format!("OAuth {access_token}"))
        .call()
        .map_err(|e| e.to_string())?;
    match response.status().as_u16() {
        200 => http::read_json(&mut response).map(Some),
        401 => Ok(None),
        status => Err(format!("vérification du jeton : HTTP {status}")),
    }
}

/// Revokes the token at Twitch (logout). Errors are ignored:
/// the token is deleted from the keyring anyway.
pub fn revoke(client_id: &str, access_token: &str) {
    let _ = http::agent()
        .post("https://id.twitch.tv/oauth2/revoke")
        .send_form([("client_id", client_id), ("token", access_token)]);
}

/// Checks tokens, renews them if needed and saves the new ones.
pub fn check(client_id: &str, tokens: Tokens) -> Outcome {
    match validate(&tokens.access_token) {
        Ok(Some(identity)) => accept(client_id, tokens, identity),
        // Expired token: renew it.
        Ok(None) => match refresh(client_id, &tokens.refresh_token) {
            Ok(Refresh::Ok(new_tokens)) => match save_tokens(&new_tokens) {
                Ok(()) => check_fresh(client_id, new_tokens),
                Err(e) => Outcome::Failed(e),
            },
            Ok(Refresh::Rejected) => {
                let _ = delete_tokens();
                Outcome::LoggedOut
            }
            Err(e) => Outcome::Failed(e),
        },
        Err(e) => Outcome::Failed(e),
    }
}

/// Like `check`, for tokens just obtained (no new renewal).
pub fn check_fresh(client_id: &str, tokens: Tokens) -> Outcome {
    match validate(&tokens.access_token) {
        Ok(Some(identity)) => accept(client_id, tokens, identity),
        Ok(None) => Outcome::LoggedOut,
        Err(e) => Outcome::Failed(e),
    }
}

fn accept(client_id: &str, tokens: Tokens, identity: Identity) -> Outcome {
    if is_usable(&identity, client_id) {
        Outcome::LoggedIn(Session { tokens, identity })
    } else {
        // Token of another application or without the chat rights: unusable.
        let _ = delete_tokens();
        Outcome::LoggedOut
    }
}

/// At start-up: reads the keyring again and checks the tokens.
pub fn restore(client_id: &str) -> Outcome {
    match load_tokens() {
        Ok(Some(tokens)) => check(client_id, tokens),
        Ok(None) => Outcome::LoggedOut,
        Err(e) => Outcome::Failed(e),
    }
}

/// The token must come from our application and have the chat rights.
fn is_usable(identity: &Identity, client_id: &str) -> bool {
    identity.client_id == client_id
        && REQUIRED_SCOPES
            .iter()
            .all(|scope| identity.scopes.iter().any(|s| s == scope))
        && crate::config::is_valid_channel(&identity.login)
}
