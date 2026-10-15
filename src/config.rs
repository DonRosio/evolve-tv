//! Application configuration, stored as JSON in `~/.config/evolve-tv/config.json`.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{paths, streamlink};

const FILE_NAME: &str = "config.json";
/// Beyond that, the file is necessarily abnormal: we refuse to read it.
const MAX_FILE_SIZE: u64 = 1024 * 1024;

// `#[derive(Serialize, Deserialize)]` automatically generates the JSON ⇄ struct conversion.
// `default`: a field missing from the file takes its default value.
// `deny_unknown_fields`: a misspelt field causes a clear error
// instead of being silently ignored.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Twitch channel to watch.
    pub channel: String,
    /// Start the stream as soon as the application opens.
    pub autostart: bool,
    /// Profile used when the current network is not associated with any profile.
    pub default_profile: String,
    pub profiles: Vec<Profile>,
    pub player: Player,
    /// Identifier of the application registered on dev.twitch.tv ("Public" type).
    /// It is not a secret: Twitch sees it on every request.
    pub twitch_client_id: Option<String>,
    /// Animated emotes in the chat (`false`: still images, less CPU).
    pub animated_emotes: bool,
    /// Buttons that send a message in one click.
    pub saved_messages: Vec<SavedMessage>,
    /// Names of the favourite emotes (bar under the chat input).
    pub favorite_emotes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedMessage {
    /// Button text (default: the beginning of the message).
    #[serde(default)]
    pub label: Option<String>,
    pub text: String,
}

/// Limits (Twitch refuses messages of more than 500 characters).
pub const MAX_MESSAGE_CHARS: usize = 500;
const MAX_LABEL_CHARS: usize = 40;
const MAX_SAVED_MESSAGES: usize = 50;
const MAX_FAVORITES: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    /// Maximum quality (`480p`, `1080p60`…): the best available quality is taken
    /// without exceeding this one (see `streamlink::candidates_up_to`).
    pub max_quality: String,
    /// Wi-Fi networks (SSID) that automatically activate this profile.
    #[serde(default)]
    pub wifi_networks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Player {
    /// Volume from 0 to 100, remembered between two launches.
    pub volume: u8,
    /// mpv's video output: `auto`, `xv` or `gpu` (see `VideoOutput`).
    pub video_output: VideoOutput,
    /// With the `gpu` output: decoding by the graphics card (less CPU and battery).
    pub hardware_decoding: bool,
    /// Imposed paths of the programs (otherwise automatic lookup).
    pub streamlink_path: Option<PathBuf>,
    pub mpv_path: Option<PathBuf>,
}

/// How mpv displays the video. Measurements on the development PC (GTX 960M):
/// Xv ≈ 87 MB at 480p but decodes with the processor (115 % of a core at 1080p60);
/// GPU ≈ 190-200 MB but almost no CPU.
// `rename_all = "lowercase"`: in the JSON, one writes "auto", "xv" or "gpu".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoOutput {
    /// Xv up to 480p, GPU above.
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

    /// Reads the configuration, or creates the file with the default values if it does not exist.
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

    /// Writes the configuration atomically (temporary file then rename):
    /// an interruption during the write cannot leave a half-written file.
    pub fn save(&self) -> Result<(), String> {
        let path = Config::path()?;
        let dir = path.parent().ok_or("chemin de configuration invalide")?;
        paths::create_private_dir(dir)
            .map_err(|e| format!("création de {} : {e}", dir.display()))?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;

        let tmp = path.with_extension("json.tmp");
        let _ = fs::remove_file(&tmp);
        let write = || -> std::io::Result<()> {
            // 0600 permissions: only the user can read or modify the file.
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

    /// Index of the profile to use for the given Wi-Fi network.
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

/// Saved message text: non-empty, 500 characters max, no control characters.
pub fn is_valid_saved_text(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= MAX_MESSAGE_CHARS
        && !text.chars().any(char::is_control)
}

/// An emote name is a word (no space) of reasonable length.
pub fn is_valid_emote_name(name: &str) -> bool {
    (1..=100).contains(&name.chars().count()) && !name.chars().any(char::is_whitespace)
}

/// A Twitch channel name only contains letters, digits and `_` (25 characters max).
/// Checking it also prevents a name like `--option` from being interpreted by streamlink.
pub fn is_valid_channel(name: &str) -> bool {
    (1..=25).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A streamlink quality: `480p`, `1080p60`, `audio_only`, `best`…
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

        // A config written before step 4 (without these fields) stays readable.
        let old: Config = serde_json::from_str(r#"{"channel":"somechannel"}"#).unwrap();
        assert_eq!(old.saved_messages.len(), 2);
    }
}
