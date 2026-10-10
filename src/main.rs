// Interdit tout code `unsafe` dans notre crate : le compilateur refusera de compiler
// s'il en trouve. Les garanties mémoire de Rust s'appliquent donc à tout notre code.
#![forbid(unsafe_code)]

mod app;
mod auth;
mod browser;
mod chat;
mod chat_column;
mod chat_panel;
mod config;
mod emoji;
mod emote_picker;
mod emotes;
mod helix;
mod http;
mod icon;
mod images;
mod irc;
mod mpv;
mod network;
mod paths;
mod streamlink;
mod video_surface;

use eframe::egui;

fn main() -> eframe::Result {
    let (config, config_error) = match config::Config::load() {
        Ok(config) => (config, None),
        Err(e) => {
            eprintln!("Configuration : {e}");
            (config::Config::default(), Some(e))
        }
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Evolve TV")
        .with_app_id("evolve-tv")
        .with_inner_size([1280.0, 720.0])
        .with_min_inner_size([640.0, 360.0]);
    if let Some(icon) = icon::load() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Evolve TV",
        options,
        Box::new(move |cc| Ok(Box::new(app::EvolveApp::new(cc, config, config_error)))),
    )
}
