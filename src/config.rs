//! Configuration de l'application, stockée en JSON dans `~/.config/evolve-tv/config.json`.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{paths, streamlink};

const FILE_NAME: &str = "config.json";
/// Au-delà, le fichier est forcément anormal : on refuse de le lire.
const MAX_FILE_SIZE: u64 = 1024 * 1024;

// `#[derive(Serialize, Deserialize)]` génère automatiquement la conversion JSON ⇄ struct.
// `default` : un champ absent du fichier prend sa valeur par défaut.
// `deny_unknown_fields` : un champ mal orthographié provoque une erreur claire
// au lieu d'être ignoré en silence.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Chaîne Twitch à regarder.
    pub channel: String,
    /// Lancer le stream dès l'ouverture de l'application.
    pub autostart: bool,
    /// Profil utilisé quand le réseau actuel n'est associé à aucun profil.
    pub default_profile: String,
    pub profiles: Vec<Profile>,
    pub player: Player,
    /// Identifiant de l'application enregistrée sur dev.twitch.tv (type « Public »).
    /// Ce n'est pas un secret : Twitch le voit à chaque requête.
    pub twitch_client_id: Option<String>,
    /// Emotes animées dans le chat (`false` : images fixes, moins de CPU).
    pub animated_emotes: bool,
    /// Boutons qui envoient un message en un clic.
    pub saved_messages: Vec<SavedMessage>,
    /// Noms des emotes favorites (barre sous la saisie du chat).
    pub favorite_emotes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedMessage {
    /// Texte du bouton (par défaut : le début du message).
    #[serde(default)]
    pub label: Option<String>,
    pub text: String,
}

/// Limites (Twitch refuse les messages de plus de 500 caractères).
pub const MAX_MESSAGE_CHARS: usize = 500;
const MAX_LABEL_CHARS: usize = 40;
const MAX_SAVED_MESSAGES: usize = 50;
const MAX_FAVORITES: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    /// Qualité maximale (`480p`, `1080p60`…) : on prend la meilleure qualité disponible
    /// sans dépasser celle-ci (voir `streamlink::candidates_up_to`).
    pub max_quality: String,
    /// Réseaux Wi-Fi (SSID) qui activent automatiquement ce profil.
    #[serde(default)]
    pub wifi_networks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Player {
    /// Volume de 0 à 100, mémorisé entre deux lancements.
    pub volume: u8,
    /// Sortie vidéo de mpv : `auto`, `xv` ou `gpu` (voir `VideoOutput`).
    pub video_output: VideoOutput,
    /// En sortie `gpu` : décodage par la carte graphique (moins de CPU et de batterie).
    pub hardware_decoding: bool,
    /// Chemins imposés des programmes (sinon recherche automatique).
    pub streamlink_path: Option<PathBuf>,
    pub mpv_path: Option<PathBuf>,
}

/// Comment mpv affiche la vidéo. Mesures sur le PC de développement (GTX 960M) :
/// Xv ≈ 87 Mo en 480p mais décode avec le processeur (115 % d'un cœur en 1080p60) ;
/// GPU ≈ 190-200 Mo mais quasiment sans CPU.
// `rename_all = "lowercase"` : dans le JSON, on écrit "auto", "xv" ou "gpu".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoOutput {
    /// Xv jusqu'à 480p, GPU au-dessus.
    #[default]
    Auto,
    Xv,
    Gpu,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            channel: "twitch".into(),
            autostart: true,
            default_profile: "5G".into(),
            profiles: vec![
                Profile {
                    name: "5G".into(),
                    max_quality: "480p".into(),
                    wifi_networks: Vec::new(),
                },
                Profile {
                    name: "Wi-Fi".into(),
                    max_quality: "1080p60".into(),
                    wifi_networks: Vec::new(),
                },
            ],
            player: Player::default(),
            twitch_client_id: None,
            animated_emotes: true,
            saved_messages: vec![
                SavedMessage {
                    label: None,
                    text: "!join".into(),
                },
                SavedMessage {
                    label: Some("monkeyLick à tous".into()),
                    text: "monkeyLick à tous monkeyLick à tous monkeyLick à tous".into(),
                },
            ],
            favorite_emotes: Vec::new(),
        }
    }
}

impl Default for Player {
    fn default() -> Self {
        Player {
            volume: 70,
            video_output: VideoOutput::Auto,
            hardware_decoding: true,
            streamlink_path: None,
            mpv_path: None,
        }
    }
}

impl Config {
    fn path() -> Result<PathBuf, String> {
        paths::config_dir()
            .map(|dir| dir.join(FILE_NAME))
            .ok_or_else(|| "impossible de trouver le dossier de configuration (HOME absent)".into())
    }

    /// Lit la configuration, ou crée le fichier avec les valeurs par défaut s'il n'existe pas.
    pub fn load() -> Result<Config, String> {
        let path = Config::path()?;
        if !path.exists() {
            let config = Config::default();
            config.save()?;
            return Ok(config);
        }
        let mut text = String::new();
        fs::File::open(&path)
            .and_then(|file| file.take(MAX_FILE_SIZE + 1).read_to_string(&mut text))
            .map_err(|e| format!("lecture de {} : {e}", path.display()))?;
        if text.len() as u64 > MAX_FILE_SIZE {
            return Err(format!("{} est trop volumineux", path.display()));
        }
        let config: Config = serde_json::from_str(&text)
            .map_err(|e| format!("{} invalide : {e}", path.display()))?;
        config
            .validate()
            .map_err(|e| format!("{} : {e}", path.display()))?;
        Ok(config)
    }

    /// Écrit la configuration de façon atomique (fichier temporaire puis renommage) :
    /// une coupure pendant l'écriture ne peut pas laisser un fichier à moitié écrit.
    pub fn save(&self) -> Result<(), String> {
        let path = Config::path()?;
        let dir = path.parent().ok_or("chemin de configuration invalide")?;
        paths::create_private_dir(dir)
            .map_err(|e| format!("création de {} : {e}", dir.display()))?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;

        let tmp = path.with_extension("json.tmp");
        let _ = fs::remove_file(&tmp);
        let write = || -> std::io::Result<()> {
            // Droits 0600 : seul l'utilisateur peut lire ou modifier le fichier.
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&tmp, &path)
        };
        write().map_err(|e| format!("écriture de {} : {e}", path.display()))
    }

    fn validate(&self) -> Result<(), String> {
        if !is_valid_channel(&self.channel) {
            return Err(format!("nom de chaîne invalide : « {} »", self.channel));
        }
        if self.profiles.is_empty() {
            return Err("il faut au moins un profil".into());
        }
        for (i, profile) in self.profiles.iter().enumerate() {
            if profile.name.trim().is_empty() {
                return Err("un profil n'a pas de nom".into());
            }
            if self.profiles[..i].iter().any(|p| p.name == profile.name) {
                return Err(format!("le profil « {} » existe en double", profile.name));
            }
            if streamlink::parse_resolution(&profile.max_quality).is_none() {
                return Err(format!(
                    "max_quality « {} » du profil « {} » invalide (exemples : 480p, 720p60, 1080p60)",
                    profile.max_quality, profile.name
                ));
            }
        }
        if !self.profiles.iter().any(|p| p.name == self.default_profile) {
            return Err(format!(
                "default_profile « {} » ne correspond à aucun profil",
                self.default_profile
            ));
        }
        if let Some(id) = &self.twitch_client_id
            && (!(20..=64).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            return Err(format!("twitch_client_id invalide : « {id} »"));
        }
        if self.player.volume > 100 {
            return Err("le volume doit être compris entre 0 et 100".into());
        }
        if self.saved_messages.len() > MAX_SAVED_MESSAGES {
            return Err(format!(
                "{MAX_SAVED_MESSAGES} messages enregistrés au maximum"
            ));
        }
        for message in &self.saved_messages {
            if !is_valid_saved_text(&message.text) {
                return Err(format!(
                    "message enregistré invalide (vide, trop long ou caractère de contrôle) : « {} »",
                    message.text
                ));
            }
            if let Some(label) = &message.label
                && (label.trim().is_empty() || label.chars().count() > MAX_LABEL_CHARS)
            {
                return Err(format!("libellé de bouton invalide : « {label} »"));
            }
        }
        if self.favorite_emotes.len() > MAX_FAVORITES {
            return Err(format!("{MAX_FAVORITES} emotes favorites au maximum"));
        }
        if let Some(name) = self
            .favorite_emotes
            .iter()
            .find(|n| !is_valid_emote_name(n))
        {
            return Err(format!("nom d'emote favorite invalide : « {name} »"));
        }
        Ok(())
    }

    /// Index du profil à utiliser pour le réseau Wi-Fi donné.
    pub fn profile_for_network(&self, ssid: Option<&str>) -> usize {
        let by_network = ssid.and_then(|ssid| {
            self.profiles
                .iter()
                .position(|p| p.wifi_networks.iter().any(|n| n == ssid))
        });
        by_network
            .or_else(|| {
                self.profiles
                    .iter()
                    .position(|p| p.name == self.default_profile)
            })
            .unwrap_or(0)
    }
}

/// Texte de message enregistré : non vide, 500 caractères max, sans caractère de contrôle.
pub fn is_valid_saved_text(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= MAX_MESSAGE_CHARS
        && !text.chars().any(char::is_control)
}

/// Un nom d'emote est un mot (sans espace), de longueur raisonnable.
pub fn is_valid_emote_name(name: &str) -> bool {
    (1..=100).contains(&name.chars().count()) && !name.chars().any(char::is_whitespace)
}

/// Un nom de chaîne Twitch ne contient que lettres, chiffres et `_` (25 caractères max).
/// Le vérifier empêche aussi qu'un nom comme `--option` soit interprété par streamlink.
pub fn is_valid_channel(name: &str) -> bool {
    (1..=25).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Une qualité streamlink : `480p`, `1080p60`, `audio_only`, `best`…
pub fn is_valid_quality(name: &str) -> bool {
    (1..=20).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_par_defaut_valide_et_relisible() {
        let config = Config::default();
        assert!(config.validate().is_ok());
        let json = serde_json::to_string_pretty(&config).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.saved_messages, config.saved_messages);
        assert_eq!(back.saved_messages[0].text, "!join");
    }

    #[test]
    fn raccourcis_invalides_refuses() {
        let mut config = Config::default();
        config.saved_messages.push(SavedMessage {
            label: None,
            text: "ligne1\nPRIVMSG #autre :injection".into(),
        });
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.favorite_emotes.push("deux mots".into());
        assert!(config.validate().is_err());

        // Une config écrite avant l'étape 4 (sans ces champs) reste lisible.
        let old: Config = serde_json::from_str(r#"{"channel":"somechannel"}"#).unwrap();
        assert_eq!(old.saved_messages.len(), 2);
    }
}
