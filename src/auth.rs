//! Connexion au compte Twitch par « Device Code Flow » et stockage du jeton dans
//! le trousseau GNOME (Secret Service).
//!
//! Déroulement : l'application demande un code à Twitch, tu le valides sur
//! twitch.tv/activate dans ton navigateur, et l'application reçoit un jeton
//! d'accès (valable ~4 h) et un jeton de renouvellement. Aucun mot de passe ni
//! secret ne transite par l'application.

use std::sync::OnceLock;

use keyring_core::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};

use crate::http;

/// Droits demandés : lire et écrire dans le chat, connaître tes emotes.
pub const SCOPES: &str = "chat:read chat:edit user:read:emotes";
const REQUIRED_SCOPES: [&str; 2] = ["chat:read", "chat:edit"];

const KEYRING_SERVICE: &str = "evolve-tv";
const KEYRING_ACCOUNT: &str = "twitch";

/// Jetons OAuth. Volontairement sans `Debug` : impossible de les afficher par erreur.
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
}

/// Identité du compte, renvoyée par Twitch quand on vérifie le jeton.
#[derive(Clone, Debug, Deserialize)]
pub struct Identity {
    pub login: String,
    pub user_id: String,
    pub client_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// Session connectée : jetons + identité.
#[derive(Clone)]
pub struct Session {
    pub tokens: Tokens,
    pub identity: Identity,
}

pub enum Outcome {
    LoggedIn(Session),
    /// Pas de jeton, ou jeton révoqué : il faut se (re)connecter.
    LoggedOut,
    /// Erreur temporaire (réseau…) : les jetons sont conservés, on réessaiera.
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

/// Réponse d'erreur de id.twitch.tv : `{"status":400,"message":"authorization_pending"}`.
#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

// ---------- Trousseau ----------

fn keyring_entry() -> Result<Entry, String> {
    // Le trousseau est initialisé une seule fois, au premier usage.
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

/// Étape 1 : demande un code à afficher à l'utilisateur.
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
    // N'ouvre jamais dans le navigateur une adresse qui ne serait pas celle de Twitch.
    if !code
        .verification_uri
        .starts_with("https://www.twitch.tv/activate")
    {
        return Err("adresse de validation inattendue".into());
    }
    Ok(code)
}

/// Étape 2 : demande (toutes les `interval` secondes) si l'utilisateur a validé le code.
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

/// Résultat d'un renouvellement de jeton.
enum Refresh {
    Ok(Tokens),
    /// Jeton de renouvellement refusé : il faut se reconnecter.
    Rejected,
}

fn refresh(client_id: &str, refresh_token: &str) -> Result<Refresh, String> {
    // Client « Public » : pas de secret, seulement l'identifiant de l'application.
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

/// Vérifie le jeton auprès de Twitch (obligatoire au démarrage puis toutes les heures).
/// `Ok(None)` : jeton expiré ou révoqué.
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

/// Révoque le jeton chez Twitch (déconnexion). Les erreurs sont ignorées :
/// le jeton est de toute façon supprimé du trousseau.
pub fn revoke(client_id: &str, access_token: &str) {
    let _ = http::agent()
        .post("https://id.twitch.tv/oauth2/revoke")
        .send_form([("client_id", client_id), ("token", access_token)]);
}

/// Vérifie des jetons, les renouvelle si besoin et enregistre les nouveaux.
pub fn check(client_id: &str, tokens: Tokens) -> Outcome {
    match validate(&tokens.access_token) {
        Ok(Some(identity)) => accept(client_id, tokens, identity),
        // Jeton expiré : on le renouvelle.
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

/// Comme `check`, pour des jetons tout juste obtenus (pas de nouveau renouvellement).
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
        // Jeton d'une autre application ou sans les droits du chat : inutilisable.
        let _ = delete_tokens();
        Outcome::LoggedOut
    }
}

/// Au démarrage : relit le trousseau et vérifie les jetons.
pub fn restore(client_id: &str) -> Outcome {
    match load_tokens() {
        Ok(Some(tokens)) => check(client_id, tokens),
        Ok(None) => Outcome::LoggedOut,
        Err(e) => Outcome::Failed(e),
    }
}

/// Le jeton doit venir de notre application et avoir les droits du chat.
fn is_usable(identity: &Identity, client_id: &str) -> bool {
    identity.client_id == client_id
        && REQUIRED_SCOPES
            .iter()
            .all(|scope| identity.scopes.iter().any(|s| s == scope))
        && crate::config::is_valid_channel(&identity.login)
}
