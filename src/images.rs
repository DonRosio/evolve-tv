//! Loading images (emotes, badges): disk cache, download and decoding
//! in background threads, then sending to the graphics card as a "texture".
//!
//! An animated image is stored as a "sheet" (sprite sheet): all its frames are
//! laid out in a grid in a single texture, and the display picks the right cell.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, ColorImage, Rect, TextureHandle, TextureId, TextureOptions, Vec2, pos2, vec2,
};
use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, Limits, RgbaImage};

use crate::{http, paths};

/// Allowed sites: we never download an image from anywhere else.
const ALLOWED_PREFIXES: [&str; 5] = [
    crate::emoji::TWEMOJI_BASE,
    "https://static-cdn.jtvnw.net/",
    "https://cdn.7tv.app/emote/",
    "https://cdn.betterttv.net/emote/",
    "https://cdn.frankerfacez.com/emote/",
];
const MAX_DOWNLOAD: u64 = 2 * 1024 * 1024;
/// Limits against booby-trapped images ("decompression bombs").
const MAX_SIDE: u32 = 256;
const MAX_FRAMES: usize = 300;
const MAX_DECODED_BYTES: usize = 32 * 1024 * 1024;
const WORKERS: usize = 2;
/// An image unused for this long is removed from memory.
const FORGET_AFTER: Duration = Duration::from_secs(5 * 60);
const SWEEP_EVERY: Duration = Duration::from_secs(60);

/// Image ready to display.
pub struct Picture {
    texture: TextureHandle,
    frame_size: Vec2,
    columns: usize,
    rows: usize,
    /// End of each frame, in seconds since the start of the animation.
    frame_ends: Vec<f64>,
}

/// What to draw at a given instant.
pub struct Frame {
    pub texture: TextureId,
    /// Part of the texture to display (coordinates from 0 to 1).
    pub uv: Rect,
    pub size: Vec2,
    /// Seconds before the next frame (`None`: still image).
    pub next_change: Option<f64>,
}

impl Picture {
    pub fn frame_at(&self, time: f64, animate: bool) -> Frame {
        let total = self.frame_ends.last().copied().unwrap_or(0.0);
        let (index, next_change) = if animate && self.frame_ends.len() > 1 && total > 0.0 {
            let t = time % total;
            let index = self
                .frame_ends
                .partition_point(|end| *end <= t)
                .min(self.frame_ends.len() - 1);
            (index, Some(self.frame_ends[index] - t))
        } else {
            (0, None)
        };
        let cell = vec2(1.0 / self.columns as f32, 1.0 / self.rows as f32);
        let (column, row) = (index % self.columns, index / self.columns);
        Frame {
            texture: self.texture.id(),
            uv: Rect::from_min_size(pos2(column as f32 * cell.x, row as f32 * cell.y), cell),
            size: self.frame_size,
            next_change,
        }
    }
}

enum Slot {
    Loading,
    Ready {
        picture: Picture,
        last_used: Instant,
    },
    Failed(Instant),
}

pub struct ImageCache {
    slots: HashMap<String, Slot>,
    jobs: Sender<String>,
    results: Receiver<(String, Option<Picture>)>,
    last_sweep: Instant,
}

impl ImageCache {
    pub fn new(ctx: &egui::Context) -> ImageCache {
        let (jobs, job_receiver) = mpsc::channel::<String>();
        let (result_sender, results) = mpsc::channel();
        // Several threads share the same work queue (protected by a lock).
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
            last_sweep: Instant::now(),
        }
    }

    /// Collects the finished images and forgets those that are no longer used.
    pub fn poll(&mut self) {
        while let Ok((url, picture)) = self.results.try_recv() {
            let slot = match picture {
                Some(picture) => Slot::Ready {
                    picture,
                    last_used: Instant::now(),
                },
                None => Slot::Failed(Instant::now()),
            };
            self.slots.insert(url, slot);
        }
        if self.last_sweep.elapsed() >= SWEEP_EVERY {
            self.last_sweep = Instant::now();
            // Dropping a `TextureHandle` frees the graphics card's memory.
            self.slots.retain(|_, slot| match slot {
                Slot::Loading => true,
                Slot::Ready { last_used, .. } => last_used.elapsed() < FORGET_AFTER,
                // A failure is retried later (passing network interruption…).
                Slot::Failed(at) => at.elapsed() < FORGET_AFTER * 2,
            });
        }
    }

    /// Image if it is ready; otherwise starts loading it and returns `None`.
    pub fn get(&mut self, url: &str) -> Option<&Picture> {
        if !self.slots.contains_key(url) {
            let allowed = ALLOWED_PREFIXES.iter().any(|p| url.starts_with(p));
            let slot = if allowed && self.jobs.send(url.to_owned()).is_ok() {
                Slot::Loading
            } else {
                Slot::Failed(Instant::now())
            };
            self.slots.insert(url.to_owned(), slot);
        }
        match self.slots.get_mut(url) {
            Some(Slot::Ready { picture, last_used }) => {
                *last_used = Instant::now();
                Some(picture)
            }
            _ => None,
        }
    }
}

fn worker(
    jobs: &Mutex<Receiver<String>>,
    results: &Sender<(String, Option<Picture>)>,
    ctx: &egui::Context,
    cache_dir: Option<&Path>,
) {
    loop {
        // The lock is only held for the time it takes to pick up a job.
        let job = jobs.lock().ok().and_then(|receiver| receiver.recv().ok());
        let Some(url) = job else { return };
        let picture = load(&url, cache_dir)
            .map_err(|e| eprintln!("Image {url} : {e}"))
            .ok()
            .map(|decoded| Picture {
                texture: ctx.load_texture(&url, decoded.sheet, TextureOptions::LINEAR),
                frame_size: decoded.frame_size,
                columns: decoded.columns,
                rows: decoded.rows,
                frame_ends: decoded.frame_ends,
            });
        if results.send((url, picture)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

struct Decoded {
    sheet: ColorImage,
    frame_size: Vec2,
    columns: usize,
    rows: usize,
    frame_ends: Vec<f64>,
}

fn load(url: &str, cache_dir: Option<&Path>) -> Result<Decoded, String> {
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

/// File name derived from the address (64-bit FNV-1a hash, in hexadecimal):
/// no character of the address ends up in the file's path.
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

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODED_BYTES as u64);
    limits
}

fn check_size((width, height): (u32, u32)) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("dimensions refusées : {width}×{height}"));
    }
    Ok(())
}

/// Decodes a still (PNG, WebP) or animated (GIF, WebP) image into a sheet of frames.
fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    let err = |e: image::ImageError| e.to_string();
    let format = image::guess_format(bytes).map_err(err)?;
    let frames = match format {
        ImageFormat::Png => {
            let mut decoder =
                image::codecs::png::PngDecoder::new(Cursor::new(bytes)).map_err(err)?;
            check_size(decoder.dimensions())?;
            decoder.set_limits(limits()).map_err(err)?;
            vec![(
                DynamicImage::from_decoder(decoder)
                    .map_err(err)?
                    .into_rgba8(),
                0.0,
            )]
        }
        ImageFormat::Gif => {
            let mut decoder = GifDecoder::new(Cursor::new(bytes)).map_err(err)?;
            check_size(decoder.dimensions())?;
            decoder.set_limits(limits()).map_err(err)?;
            collect_frames(decoder.into_frames())?
        }
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(err)?;
            check_size(decoder.dimensions())?;
            decoder.set_limits(limits()).map_err(err)?;
            if decoder.has_animation() {
                collect_frames(decoder.into_frames())?
            } else {
                vec![(
                    DynamicImage::from_decoder(decoder)
                        .map_err(err)?
                        .into_rgba8(),
                    0.0,
                )]
            }
        }
        _ => return Err("format d'image non pris en charge".into()),
    };
    build_sheet(frames)
}

/// Reads the frames of an animation with their duration (in seconds).
fn collect_frames(frames: image::Frames<'_>) -> Result<Vec<(RgbaImage, f64)>, String> {
    let mut out = Vec::new();
    let mut total_bytes = 0;
    for frame in frames.take(MAX_FRAMES) {
        let frame = frame.map_err(|e| e.to_string())?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let mut ms = f64::from(numerator) / f64::from(denominator.max(1));
        // Browser convention: a near-zero duration counts as 100 ms.
        if ms < 20.0 {
            ms = 100.0;
        }
        let buffer = frame.into_buffer();
        total_bytes += buffer.len();
        if total_bytes > MAX_DECODED_BYTES {
            return Err("animation trop lourde".into());
        }
        out.push((buffer, ms / 1000.0));
    }
    if out.is_empty() {
        return Err("animation vide".into());
    }
    Ok(out)
}

/// Lays the frames out in a grid in a single image.
fn build_sheet(frames: Vec<(RgbaImage, f64)>) -> Result<Decoded, String> {
    let (width, height) = frames[0].0.dimensions();
    if frames
        .iter()
        .any(|(frame, _)| frame.dimensions() != (width, height))
    {
        return Err("frames de tailles différentes".into());
    }
    let (w, h) = (width as usize, height as usize);
    let count = frames.len();
    let columns = (count as f64).sqrt().ceil() as usize;
    let rows = count.div_ceil(columns);
    let sheet_width = w * columns;
    let mut rgba = vec![0u8; sheet_width * h * rows * 4];
    let mut frame_ends = Vec::with_capacity(count);
    let mut elapsed = 0.0;
    for (i, (frame, duration)) in frames.iter().enumerate() {
        let (column, row) = (i % columns, i / columns);
        for y in 0..h {
            let source = &frame.as_raw()[y * w * 4..(y + 1) * w * 4];
            let start = ((row * h + y) * sheet_width + column * w) * 4;
            rgba[start..start + w * 4].copy_from_slice(source);
        }
        elapsed += duration;
        frame_ends.push(elapsed);
    }
    Ok(Decoded {
        sheet: ColorImage::from_rgba_unmultiplied([sheet_width, h * rows], &rgba),
        frame_size: vec2(width as f32, height as f32),
        columns,
        rows,
        frame_ends,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nom_de_cache_sans_caracteres_dangereux() {
        let name = cache_name("https://static-cdn.jtvnw.net/../../etc/passwd");
        assert_eq!(name.len(), 20);
        assert!(
            name.chars()
                .all(|c| c.is_ascii_hexdigit() || c == '.' || c.is_ascii_lowercase())
        );
    }

    #[test]
    fn planche_et_frames() {
        let frame = |v: u8| {
            (
                RgbaImage::from_pixel(2, 2, image::Rgba([v, 0, 0, 255])),
                0.1,
            )
        };
        let decoded = build_sheet(vec![frame(1), frame(2), frame(3)]).unwrap();
        // 3 frames → grille de 2 colonnes × 2 lignes.
        assert_eq!((decoded.columns, decoded.rows), (2, 2));
        assert_eq!(decoded.sheet.size, [4, 4]);
        // Top-left pixel of the 2nd frame (column 1, row 0).
        assert_eq!(decoded.sheet.pixels[2].r(), 2);
        assert_eq!(decoded.frame_ends.len(), 3);
        assert!((decoded.frame_ends[2] - 0.3).abs() < 1e-9);
    }

    #[test]
    fn refus_des_images_geantes() {
        assert!(check_size((300, 10)).is_err());
        assert!(check_size((0, 10)).is_err());
        assert!(check_size((32, 32)).is_ok());
    }
}
