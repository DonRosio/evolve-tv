//! Emotes 7TV, BTTV et FFZ : globales et propres à la chaîne.
//!
//! Ces trois services exposent des API publiques (sans compte). Les adresses des
//! images sont reconstruites ici à partir des identifiants, jamais reprises telles
//! quelles des réponses : impossible de nous faire télécharger une image ailleurs.

use std::collections::HashMap;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::http;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Twitch,
    SevenTv,
    Bttv,
    Ffz,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Twitch => "Twitch",
            Provider::SevenTv => "7TV",
            Provider::Bttv => "BTTV",
            Provider::Ffz => "FFZ",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Emote {
    pub name: String,
    pub url: String,
    /// Emote « zero-width » : dessinée par-dessus l'emote précédente.
    pub zero_width: bool,
    pub provider: Provider,
}

pub type EmoteMap = HashMap<String, Emote>;

pub struct Loaded {
    pub emotes: EmoteMap,
    /// Nombre d'emotes par service (pour le message de confirmation).
    pub counts: Vec<(Provider, usize)>,
    pub errors: Vec<String>,
}

/// Emotes zero-width historiques de BTTV (BTTV ne les signale pas dans son API).
const BTTV_ZERO_WIDTH: [&str; 8] = [
    "IceCold",
    "SoSnowy",
    "SantaHat",
    "TopHat",
    "ReinDeer",
    "CandyCane",
    "cvMask",
    "cvHazmat",
];

/// Charge toutes les emotes (appel bloquant : quelques requêtes HTTP).
pub fn load(room_id: &str) -> Loaded {
    type Source = fn(&str) -> Result<Vec<Emote>, String>;
    // Ordre de priorité : en cas de nom identique, la dernière source gagne
    // (les emotes de la chaîne passent devant les globales, 7TV devant BTTV et FFZ).
    let sources: [(Provider, &str, Source); 6] = [
        (Provider::Ffz, "globales", ffz_global),
        (Provider::Bttv, "globales", bttv_global),
        (Provider::SevenTv, "globales", seventv_global),
        (Provider::Ffz, "de la chaîne", ffz_channel),
        (Provider::Bttv, "de la chaîne", bttv_channel),
        (Provider::SevenTv, "de la chaîne", seventv_channel),
    ];
    let mut emotes = EmoteMap::new();
    let mut counts: Vec<(Provider, usize)> = Vec::new();
    let mut errors = Vec::new();
    for (provider, scope, source) in sources {
        match source(room_id) {
            Ok(list) => {
                match counts.iter_mut().find(|(p, _)| *p == provider) {
                    Some((_, n)) => *n += list.len(),
                    None => counts.push((provider, list.len())),
                }
                emotes.extend(list.into_iter().map(|e| (e.name.clone(), e)));
            }
            Err(e) => errors.push(format!("{} {scope} : {e}", provider.label())),
        }
    }
    Loaded {
        emotes,
        counts,
        errors,
    }
}

/// GET JSON ; `Ok(None)` si la ressource n'existe pas (chaîne sans compte 7TV…).
fn get_json<T: DeserializeOwned>(url: &str) -> Result<Option<T>, String> {
    let mut response = http::agent().get(url).call().map_err(|e| e.to_string())?;
    match response.status().as_u16() {
        200 => http::read_json(&mut response).map(Some),
        404 => Ok(None),
        status => Err(format!("HTTP {status}")),
    }
}

fn is_valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Un nom d'emote est un « mot » : ni vide, ni espace, longueur raisonnable.
fn is_valid_name(name: &str) -> bool {
    (1..=100).contains(&name.chars().count()) && !name.chars().any(char::is_whitespace)
}

fn emote(name: String, url: String, zero_width: bool, provider: Provider) -> Option<Emote> {
    is_valid_name(&name).then_some(Emote {
        name,
        url,
        zero_width,
        provider,
    })
}

// ---------- 7TV ----------

#[derive(Deserialize)]
struct SevenTvSet {
    #[serde(default)]
    emotes: Vec<SevenTvEmote>,
}

#[derive(Deserialize)]
struct SevenTvEmote {
    id: String,
    /// Nom dans ce set (la chaîne peut renommer une emote).
    name: String,
    #[serde(default)]
    flags: u32,
    data: Option<SevenTvData>,
}

#[derive(Deserialize)]
struct SevenTvData {
    #[serde(default)]
    flags: u32,
}

#[derive(Deserialize)]
struct SevenTvUser {
    emote_set: Option<SevenTvSet>,
}

fn seventv_emotes(set: SevenTvSet) -> Vec<Emote> {
    set.emotes
        .into_iter()
        .filter(|e| is_valid_id(&e.id))
        .filter_map(|e| {
            // Zero-width : drapeau du set (bit 0) ou de l'emote elle-même (bit 8).
            let zero_width =
                e.flags & 1 != 0 || e.data.as_ref().is_some_and(|d| d.flags & 256 != 0);
            let url = format!("https://cdn.7tv.app/emote/{}/1x.webp", e.id);
            emote(e.name, url, zero_width, Provider::SevenTv)
        })
        .collect()
}

fn seventv_global(_room: &str) -> Result<Vec<Emote>, String> {
    let set: Option<SevenTvSet> = get_json("https://7tv.io/v3/emote-sets/global")?;
    Ok(set.map(seventv_emotes).unwrap_or_default())
}

fn seventv_channel(room: &str) -> Result<Vec<Emote>, String> {
    let user: Option<SevenTvUser> = get_json(&format!("https://7tv.io/v3/users/twitch/{room}"))?;
    Ok(user
        .and_then(|u| u.emote_set)
        .map(seventv_emotes)
        .unwrap_or_default())
}

// ---------- BTTV ----------

#[derive(Deserialize)]
struct BttvEmote {
    id: String,
    code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BttvUser {
    #[serde(default)]
    channel_emotes: Vec<BttvEmote>,
    #[serde(default)]
    shared_emotes: Vec<BttvEmote>,
}

fn bttv_emotes(list: impl IntoIterator<Item = BttvEmote>) -> Vec<Emote> {
    list.into_iter()
        .filter(|e| is_valid_id(&e.id))
        .filter_map(|e| {
            let zero_width = BTTV_ZERO_WIDTH.contains(&e.code.as_str());
            let url = format!("https://cdn.betterttv.net/emote/{}/1x.webp", e.id);
            emote(e.code, url, zero_width, Provider::Bttv)
        })
        .collect()
}

fn bttv_global(_room: &str) -> Result<Vec<Emote>, String> {
    let list: Option<Vec<BttvEmote>> =
        get_json("https://api.betterttv.net/3/cached/emotes/global")?;
    Ok(bttv_emotes(list.unwrap_or_default()))
}

fn bttv_channel(room: &str) -> Result<Vec<Emote>, String> {
    let user: Option<BttvUser> = get_json(&format!(
        "https://api.betterttv.net/3/cached/users/twitch/{room}"
    ))?;
    Ok(user
        .map(|u| bttv_emotes(u.channel_emotes.into_iter().chain(u.shared_emotes)))
        .unwrap_or_default())
}

// ---------- FFZ ----------

#[derive(Deserialize)]
struct FfzSet {
    #[serde(default)]
    emoticons: Vec<FfzEmote>,
}

#[derive(Deserialize)]
struct FfzEmote {
    id: u64,
    name: String,
    /// Les « modificateurs » FFZ transforment l'emote précédente : non gérés, ignorés.
    #[serde(default)]
    modifier: bool,
    /// Présent seulement pour les emotes animées.
    animated: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct FfzGlobal {
    default_sets: Vec<u64>,
    sets: HashMap<String, FfzSet>,
}

#[derive(Deserialize)]
struct FfzRoom {
    #[serde(default)]
    sets: HashMap<String, FfzSet>,
}

fn ffz_emotes(sets: impl IntoIterator<Item = FfzSet>) -> Vec<Emote> {
    sets.into_iter()
        .flat_map(|set| set.emoticons)
        .filter(|e| !e.modifier)
        .filter_map(|e| {
            let url = match e.animated {
                Some(_) => format!("https://cdn.frankerfacez.com/emote/{}/animated/1", e.id),
                None => format!("https://cdn.frankerfacez.com/emote/{}/1", e.id),
            };
            emote(e.name, url, false, Provider::Ffz)
        })
        .collect()
}

fn ffz_global(_room: &str) -> Result<Vec<Emote>, String> {
    let Some(global) = get_json::<FfzGlobal>("https://api.frankerfacez.com/v1/set/global")? else {
        return Ok(Vec::new());
    };
    let FfzGlobal {
        default_sets,
        mut sets,
    } = global;
    Ok(ffz_emotes(
        default_sets
            .iter()
            .filter_map(|id| sets.remove(&id.to_string())),
    ))
}

fn ffz_channel(room: &str) -> Result<Vec<Emote>, String> {
    let room: Option<FfzRoom> =
        get_json(&format!("https://api.frankerfacez.com/v1/room/id/{room}"))?;
    Ok(room
        .map(|r| ffz_emotes(r.sets.into_values()))
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyse_7tv() {
        let json = r#"{"emotes":[
            {"id":"01ABC","name":"RainTime","flags":1,"data":{"flags":256}},
            {"id":"01DEF","name":"KEKW","flags":0,"data":{"flags":0}},
            {"id":"../x","name":"Piege","flags":0,"data":null},
            {"id":"01GHI","name":"avec espace","flags":0,"data":null}]}"#;
        let set: SevenTvSet = serde_json::from_str(json).unwrap();
        let emotes = seventv_emotes(set);
        assert_eq!(emotes.len(), 2);
        assert!(emotes[0].zero_width);
        assert_eq!(emotes[1].url, "https://cdn.7tv.app/emote/01DEF/1x.webp");
    }

    #[test]
    fn analyse_ffz() {
        let json = r#"{"emoticons":[
            {"id":9,"name":"ZrehplaR","modifier":false},
            {"id":10,"name":"Anim","animated":{"1":"x"}},
            {"id":11,"name":"Mod","modifier":true}]}"#;
        let set: FfzSet = serde_json::from_str(json).unwrap();
        let emotes = ffz_emotes([set]);
        let names: Vec<_> = emotes.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["ZrehplaR", "Anim"]);
        assert_eq!(
            emotes[1].url,
            "https://cdn.frankerfacez.com/emote/10/animated/1"
        );
    }
}
