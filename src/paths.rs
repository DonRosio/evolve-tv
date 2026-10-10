//! Emplacements des fichiers de l'application (norme XDG de Linux) et recherche des programmes.

use std::env;
use std::fs::{self, DirBuilder, Permissions};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

const APP_DIR: &str = "evolve-tv";

/// Renvoie `$VAR/evolve-tv`, ou `$HOME/<repli>/evolve-tv` si la variable est absente.
/// Les chemins relatifs sont ignorés : ils dépendraient du dossier courant.
fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(dir) = env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Some(dir.join(APP_DIR));
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())?;
    Some(home.join(fallback).join(APP_DIR))
}

/// `~/.config/evolve-tv` : configuration.
pub fn config_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// `~/.local/share/evolve-tv` : données installées (icône…).
pub fn data_dir() -> Option<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// `~/.cache/evolve-tv` : fichiers re-téléchargeables (images des emotes et badges).
pub fn cache_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

/// `/run/user/<uid>/evolve-tv` : fichiers temporaires (socket de mpv).
/// Ce dossier système est privé à l'utilisateur et vidé à la déconnexion.
pub fn runtime_dir() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join(APP_DIR))
}

/// Crée un dossier accessible uniquement par l'utilisateur (droits 0700).
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // Si le dossier existait déjà avec des droits plus larges, on les resserre.
    fs::set_permissions(dir, Permissions::from_mode(0o700))
}

/// Cherche un exécutable : chemin imposé par la config, sinon le PATH, sinon `~/.local/bin`.
/// Les entrées relatives du PATH (comme `.`) sont ignorées : un fichier piégé dans le
/// dossier courant ne pourrait pas se faire passer pour streamlink ou mpv.
pub fn find_program(name: &str, configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return is_executable(path).then(|| path.to_path_buf());
    }
    let mut candidates: Vec<PathBuf> = env::var_os("PATH")
        .map(|paths| {
            env::split_paths(&paths)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(name))
                .collect()
        })
        .unwrap_or_default();
    if let Some(home) = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        candidates.push(home.join(".local/bin").join(name));
    }
    candidates.into_iter().find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
