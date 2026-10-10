//! Chargement des images (emotes, badges) : cache disque, téléchargement et décodage
//! dans des threads de fond, puis envoi à la carte graphique sous forme de « texture ».

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use image::{ImageFormat, ImageReader, Limits};

use crate::{http, paths};

/// Sites autorisés : on ne télécharge jamais une image ailleurs.
const ALLOWED_PREFIXES: [&str; 1] = ["https://static-cdn.jtvnw.net/"];
const MAX_DOWNLOAD: u64 = 2 * 1024 * 1024;
const WORKERS: usize = 2;

enum Slot {
    Loading,
    Ready(TextureHandle),
    Failed,
}

pub struct ImageCache {
    slots: HashMap<String, Slot>,
    jobs: Sender<String>,
    results: Receiver<(String, Option<TextureHandle>)>,
}

impl ImageCache {
    pub fn new(ctx: &egui::Context) -> ImageCache {
        let (jobs, job_receiver) = mpsc::channel::<String>();
        let (result_sender, results) = mpsc::channel();
        // Plusieurs threads se partagent la même file de travail (protégée par un verrou).
        let job_receiver = Arc::new(Mutex::new(job_receiver));
        let cache_dir = paths::cache_dir().map(|dir| dir.join("images"));
        for _ in 0..WORKERS {
            let (jobs, results, ctx, dir) = (
                job_receiver.clone(),
                result_sender.clone(),
                ctx.clone(),
                cache_dir.clone(),
            );
            thread::spawn(move || worker(&jobs, &results, &ctx, dir.as_deref()));
        }
        ImageCache {
            slots: HashMap::new(),
            jobs,
            results,
        }
    }

    /// Récupère les images terminées par les threads (à appeler à chaque image).
    pub fn poll(&mut self) {
        while let Ok((url, texture)) = self.results.try_recv() {
            let slot = texture.map_or(Slot::Failed, Slot::Ready);
            self.slots.insert(url, slot);
        }
    }

    /// Texture de l'image si elle est prête ; sinon lance son chargement et renvoie `None`.
    pub fn get(&mut self, url: &str) -> Option<&TextureHandle> {
        if !self.slots.contains_key(url) {
            let allowed = ALLOWED_PREFIXES.iter().any(|p| url.starts_with(p));
            if allowed && self.jobs.send(url.to_owned()).is_ok() {
                self.slots.insert(url.to_owned(), Slot::Loading);
            } else {
                self.slots.insert(url.to_owned(), Slot::Failed);
            }
        }
        match self.slots.get(url) {
            Some(Slot::Ready(texture)) => Some(texture),
            _ => None,
        }
    }
}

fn worker(
    jobs: &Mutex<Receiver<String>>,
    results: &Sender<(String, Option<TextureHandle>)>,
    ctx: &egui::Context,
    cache_dir: Option<&Path>,
) {
    loop {
        // Le verrou n'est tenu que le temps de prendre un travail.
        let job = jobs.lock().ok().and_then(|receiver| receiver.recv().ok());
        let Some(url) = job else { return };
        let texture = load(&url, cache_dir)
            .map_err(|e| eprintln!("Image {url} : {e}"))
            .ok()
            .map(|image| ctx.load_texture(&url, image, TextureOptions::LINEAR));
        if results.send((url, texture)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

fn load(url: &str, cache_dir: Option<&Path>) -> Result<ColorImage, String> {
    let cached = cache_dir.map(|dir| dir.join(cache_name(url)));
    let bytes = match cached.as_deref().and_then(|path| fs::read(path).ok()) {
        Some(bytes) => bytes,
        None => {
            let bytes = http::get_bytes(url, MAX_DOWNLOAD)?;
            if let (Some(dir), Some(path)) = (cache_dir, &cached) {
                let _ = write_cache(dir, path, &bytes);
            }
            bytes
        }
    };
    decode(&bytes)
}

/// Nom de fichier dérivé de l'adresse (empreinte FNV-1a 64 bits, en hexadécimal) :
/// aucun caractère de l'adresse ne se retrouve dans le chemin du fichier.
fn cache_name(url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}.img")
}

fn write_cache(dir: &Path, path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    paths::create_private_dir(dir)?;
    let tmp =
        path.with_extension(format!("tmp{:?}", thread::current().id()).replace(['(', ')'], ""));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    fs::rename(&tmp, path)
}

/// Décode une image en limitant sa taille (protection contre les images piégées).
fn decode(bytes: &[u8]) -> Result<ColorImage, String> {
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if reader.format() != Some(ImageFormat::Png) {
        return Err("format d'image non pris en charge".into());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(512);
    limits.max_image_height = Some(512);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::cache_name;

    #[test]
    fn nom_de_cache_sans_caracteres_dangereux() {
        let name = cache_name("https://static-cdn.jtvnw.net/../../etc/passwd");
        assert_eq!(name.len(), 20);
        assert!(
            name.chars()
                .all(|c| c.is_ascii_hexdigit() || c == '.' || c.is_ascii_lowercase())
        );
    }
}
