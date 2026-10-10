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
}

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
    /// Décodage vidéo par la carte graphique : moins de CPU et de batterie.
    pub hardware_decoding: bool,
    /// Chemins imposés des programmes (sinon recherche automatique).
    pub streamlink_path: Option<PathBuf>,
    pub mpv_path: Option<PathBuf>,
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
        }
    }
}

impl Default for Player {
    fn default() -> Self {
        Player {
            volume: 70,
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
        if self.player.volume > 100 {
            return Err("le volume doit être compris entre 0 et 100".into());
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
