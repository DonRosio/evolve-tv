//! Calls to Twitch's "Helix" API: chat badges and the user's emotes.

use std::collections::HashMap;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::chat::Badge;
use crate::http;

/// Image and name of a badge ("Subscriber for 1 year"…).
#[derive(Clone)]
pub struct BadgeImage {
    pub url: String,
    pub title: String,
}

#[derive(Deserialize)]
struct Page<T> {
    data: Vec<T>,
    #[serde(default)]
    pagination: Pagination,
}

#[derive(Deserialize, Default)]
struct Pagination {
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct BadgeSet {
    set_id: String,
    versions: Vec<BadgeVersion>,
}

#[derive(Deserialize)]
struct BadgeVersion {
    id: String,
    image_url_1x: String,
    title: String,
}

#[derive(Deserialize)]
struct UserEmote {
    id: String,
    name: String,
}

fn get<T: DeserializeOwned>(
    client_id: &str,
    token: &str,
    url: &str,
    query: &[(&str, &str)],
) -> Result<Page<T>, String> {
    let mut request = http::agent()
        .get(url)
        .header("Client-Id", client_id)
        .header("Authorization", format!("Bearer {token}"));
    for (key, value) in query {
        request = request.query(*key, *value);
    }
    let mut response = request.call().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("API Twitch : HTTP {}", response.status()));
    }
    http::read_json(&mut response)
}

fn is_numeric_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}

/// Global badges then the channel's (which replace the global ones, e.g. subscriber badges).
pub fn badges(
    client_id: &str,
    token: &str,
    broadcaster_id: &str,
) -> Result<HashMap<Badge, BadgeImage>, String> {
    if !is_numeric_id(broadcaster_id) {
        return Err("identifiant de chaîne invalide".into());
    }
    let global: Page<BadgeSet> = get(
        client_id,
        token,
        "https://api.twitch.tv/helix/chat/badges/global",
        &[],
    )?;
    let channel: Page<BadgeSet> = get(
        client_id,
        token,
        "https://api.twitch.tv/helix/chat/badges",
        &[("broadcaster_id", broadcaster_id)],
    )?;
    let mut badges = HashMap::new();
    for set in global.data.into_iter().chain(channel.data) {
        for version in set.versions {
            let badge = Badge {
                set: set.set_id.clone(),
                version: version.id,
            };
            let image = BadgeImage {
                url: version.image_url_1x,
                title: version.title,
            };
            badges.insert(badge, image);
        }
    }
    Ok(badges)
}

/// Twitch emotes the user can use (global, subscriptions…): name → identifier.
/// Used to show our own messages (Twitch does not send them back to us) and, later,
/// for autocompletion.
pub fn user_emotes(
    client_id: &str,
    token: &str,
    user_id: &str,
) -> Result<HashMap<String, String>, String> {
    if !is_numeric_id(user_id) {
        return Err("identifiant d'utilisateur invalide".into());
    }
    let mut emotes = HashMap::new();
    let mut cursor: Option<String> = None;
    // Page cap: protects against an infinite loop if the API answered badly.
    for _ in 0..50 {
        let mut query = vec![("user_id", user_id)];
        if let Some(after) = &cursor {
            query.push(("after", after));
        }
        let page: Page<UserEmote> = get(
            client_id,
            token,
            "https://api.twitch.tv/helix/chat/emotes/user",
            &query,
        )?;
        emotes.extend(page.data.into_iter().map(|e| (e.name, e.id)));
        match page.pagination.cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(emotes)
}
