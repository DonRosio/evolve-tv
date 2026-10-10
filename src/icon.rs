//! Icône de la fenêtre (fichier hors du dépôt git, voir .gitignore).

use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui::IconData;
use image::{ImageFormat, ImageReader, Limits};

use crate::paths;

const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;

/// Cherche `~/.local/share/evolve-tv/icon.png`, puis `assets/icon.png` du projet.
/// Sans icône, l'application fonctionne normalement.
pub fn load() -> Option<IconData> {
    let candidates = [
        paths::data_dir().map(|dir| dir.join("icon.png")),
        Some(PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icon.png"
        ))),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter(|path| path.exists())
        .find_map(|path| {
            decode(&path)
                .map_err(|e| eprintln!("Icône {} ignorée : {e}", path.display()))
                .ok()
        })
}

fn decode(path: &Path) -> Result<IconData, String> {
    let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE_SIZE {
        return Err("fichier trop volumineux".into());
    }
    let mut reader = ImageReader::open(path).map_err(|e| e.to_string())?;
    reader.set_format(ImageFormat::Png);
    // Limites contre les images piégées (« bombes de décompression ») :
    // un petit fichier qui prétendrait faire 50 000 × 50 000 pixels.
    let mut limits = Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
    Ok(IconData {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}
