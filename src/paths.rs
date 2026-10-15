//! Locations of the application's files (Linux XDG standard) and lookup of programs.

use std::env;
use std::fs::{self, DirBuilder, Permissions};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

const APP_DIR: &str = "evolve-tv";

/// Returns `$VAR/evolve-tv`, or `$HOME/<fallback>/evolve-tv` if the variable is absent.
/// Relative paths are ignored: they would depend on the current folder.
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

/// `~/.local/share/evolve-tv`: installed data (icon…).
pub fn data_dir() -> Option<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// `~/.cache/evolve-tv`: re-downloadable files (images of emotes and badges).
pub fn cache_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

/// `/run/user/<uid>/evolve-tv` : fichiers temporaires (socket de mpv).
/// This system folder is private to the user and emptied at logout.
pub fn runtime_dir() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join(APP_DIR))
}

/// Creates a folder reachable only by the user (0700 permissions).
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // If the folder already existed with wider permissions, they are tightened.
    fs::set_permissions(dir, Permissions::from_mode(0o700))
}

/// Looks for an executable: path imposed by the config, otherwise the PATH, otherwise `~/.local/bin`.
/// Relative PATH entries (like `.`) are ignored: a booby-trapped file in the
/// current folder could not pass itself off as streamlink or mpv.
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
