//! Main window: controls at the top, video in the middle, chat on the right.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Align2, Color32, FontId, Key, Layout, RichText};
use egui_phosphor::regular as icon;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::{Value, json};

use crate::chat_column::ChatColumn;
use crate::config::{Config, VideoOutput};
use crate::mpv::{self, Mpv, MpvEvent, Output, Placement};
use crate::streamlink::{self, Server, ServerEvent};
use crate::video_surface::VideoSurface;
use crate::{network, paths, style};

/// Messages sent by the background threads to the interface.
///
/// The threads never touch the application's state directly (Rust would forbid it
/// without a lock): they send a message through a channel, which the interface
/// reads at the start of each frame.
enum Event {
    Network(Option<String>),
    /// The number identifies the session: messages from an old streamlink are ignored.
    Server(u64, ServerEvent),
    /// Same for mpv instances.
    Mpv(u64, MpvEvent),
}

/// Usual Twitch qualities, offered when the channel is offline (the real list is
/// only known once the stream starts). A missing quality is replaced by the best
/// one possible for the profile.
const TYPICAL_QUALITIES: [&str; 7] = [
    "1080p60",
    "720p60",
    "720p30",
    "480p30",
    "360p30",
    "160p30",
    "audio_only",
];

/// With the `auto` video output, the maximum height displayed by Xv (above it: GPU).
const XV_MAX_HEIGHT: u32 = 480;

enum Status {
    Stopped,
    Connecting,
    Offline,
    Playing,
    Error(String),
}

/// A viewing in progress: streamlink, then mpv once the stream is ready.
struct Session {
    id: u64,
    // The fields are dropped in declaration order: mpv first, then streamlink.
    mpv: Option<(u64, Mpv)>,
    server: Server,
    /// List of qualities handed to streamlink (the first available one is played).
    qualities: Vec<String>,
    url: Option<String>,
    playing_quality: Option<String>,
    last_error: Option<String>,
    /// Reason for the last end of playback reported by mpv.
    end_reason: Option<String>,
}

pub struct EvolveApp {
    ctx: egui::Context,
    tx: Sender<Event>,
    rx: Receiver<Event>,

    config: Config,
    /// Error reading the config: the default values are used then,
    /// without ever rewriting the file, so as not to erase your changes.
    config_error: Option<String>,
    config_dirty: bool,
    streamlink_program: Option<PathBuf>,
    mpv_program: Option<PathBuf>,
    /// The desktop is drawn by the NVIDIA driver (see `display_uses_nvidia`).
    nvidia_display: bool,

    profile: usize,
    /// Quality chosen by hand (`None`: automatic, according to the profile).
    quality: Option<String>,
    available: Vec<String>,
    wifi: Option<String>,
    network_known: bool,

    status: Status,
    session: Option<Session>,
    next_id: u64,
    restarts: Vec<Instant>,

    surface: Option<VideoSurface>,
    embed_error: Option<String>,
    detached: bool,
    /// "Video only" full screen (F key).
    fullscreen: bool,
    /// Window in full screen without hiding anything (F11 key).
    window_fullscreen: bool,
    /// Theatre mode: top and bottom bars hidden (video + chat + controls).
    theater: bool,
    paused: bool,
    muted: bool,

    chat: ChatColumn,
}

impl EvolveApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        config_error: Option<String>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        style::install(&cc.egui_ctx);

        let (surface, embed_error) = match create_surface(cc) {
            Ok(surface) => (Some(surface), None),
            Err(e) => (None, Some(e)),
        };
        let streamlink_program =
            paths::find_program("streamlink", config.player.streamlink_path.as_deref());
        let mpv_program = paths::find_program("mpv", config.player.mpv_path.as_deref());

        // Wi-Fi detection in the background; the stream starts when it answers.
        let (tx_net, ctx) = (tx.clone(), cc.egui_ctx.clone());
        thread::spawn(move || {
            let _ = tx_net.send(Event::Network(network::current_wifi()));
            ctx.request_repaint();
        });

        let chat = ChatColumn::new(&cc.egui_ctx, &config);

        EvolveApp {
            chat,
            ctx: cc.egui_ctx.clone(),
            tx,
            rx,
            profile: config.profile_for_network(None),
            config,
            config_error,
            config_dirty: false,
            streamlink_program,
            mpv_program,
            nvidia_display: display_uses_nvidia(),
            quality: None,
            available: Vec::new(),
            wifi: None,
            network_known: false,
            status: Status::Stopped,
            session: None,
            next_id: 0,
            restarts: Vec::new(),
            detached: surface.is_none(),
            surface,
            embed_error,
            fullscreen: false,
            window_fullscreen: false,
            theater: false,
            paused: false,
            muted: false,
        }
    }

    // ---------- Managing the viewing ----------

    /// Starts (or restarts) streamlink with the current profile and quality.
    fn start(&mut self) {
        self.stop();
        let Some(program) = self.streamlink_program.clone() else {
            self.status = Status::Error(
                "streamlink introuvable : installe-le ou renseigne player.streamlink_path".into(),
            );
            return;
        };
        if self.mpv_program.is_none() {
            self.status =
                Status::Error("mpv introuvable : installe-le ou renseigne player.mpv_path".into());
            return;
        }
        let mut qualities: Vec<String> = self.quality.iter().cloned().collect();
        qualities.extend(streamlink::candidates_up_to(
            &self.config.profiles[self.profile].max_quality,
        ));

        self.next_id += 1;
        let id = self.next_id;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let on_event = move |event| {
            let _ = tx.send(Event::Server(id, event));
            ctx.request_repaint();
        };
        match Server::start(&program, &self.config.channel, &qualities, on_event) {
            Ok(server) => {
                self.session = Some(Session {
                    id,
                    mpv: None,
                    server,
                    qualities,
                    url: None,
                    playing_quality: None,
                    last_error: None,
                    end_reason: None,
                });
                self.status = Status::Connecting;
            }
            Err(e) => self.status = Status::Error(e),
        }
    }

    fn stop(&mut self) {
        // Dropping the session stops mpv and streamlink (see their `impl Drop`).
        self.session = None;
        self.status = Status::Stopped;
        self.paused = false;
    }

    /// Starts (or restarts) mpv on the session's stream, embedded or detached.
    fn launch_mpv(&mut self) {
        // Quality played if streamlink has already announced it, otherwise the one it will pick.
        let Some(session) = &self.session else {
            return;
        };
        let quality = session
            .playing_quality
            .clone()
            .or_else(|| streamlink::expected_choice(&session.qualities, &self.available));
        let output = self.output_for(quality.as_deref());

        let Some(session) = &mut self.session else {
            return;
        };
        let Some(url) = session.url.clone() else {
            return;
        };
        session.mpv = None; // close the old mpv if there is one
        session.end_reason = None;
        self.paused = false;

        let Some(program) = &self.mpv_program else {
            return;
        };
        let Some(dir) = paths::runtime_dir() else {
            self.status = Status::Error(
                "XDG_RUNTIME_DIR absent : impossible de créer le socket de mpv".into(),
            );
            return;
        };
        if let Err(e) = paths::create_private_dir(&dir) {
            self.status = Status::Error(format!("création de {} : {e}", dir.display()));
            return;
        }
        self.next_id += 1;
        let mpv_id = self.next_id;
        let socket = dir.join(format!("mpv-{}-{mpv_id}.sock", std::process::id()));
        let placement = match (&self.surface, self.detached) {
            (Some(surface), false) => Placement::Embedded(surface.id()),
            _ => Placement::Detached,
        };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let on_event = move |event| {
            let _ = tx.send(Event::Mpv(mpv_id, event));
            ctx.request_repaint();
        };
        let options = mpv::Options {
            program,
            url: &url,
            placement,
            output,
            volume: self.config.player.volume,
            socket,
        };
        match Mpv::spawn(options, on_event) {
            Ok(mpv) => session.mpv = Some((mpv_id, mpv)),
            Err(e) => self.status = Status::Error(e),
        }
    }

    /// Video output suited to a given quality.
    fn output_for(&self, quality: Option<&str>) -> Output {
        let gpu = Output::Gpu {
            hardware_decoding: self.config.player.hardware_decoding,
            nvidia_only: self.nvidia_display,
        };
        match self.config.player.video_output {
            VideoOutput::Xv => Output::Xv,
            VideoOutput::Gpu => gpu,
            VideoOutput::Auto => {
                let small = quality.is_some_and(|q| {
                    q == "audio_only"
                        || streamlink::parse_resolution(q).is_some_and(|(h, _)| h <= XV_MAX_HEIGHT)
                });
                if small { Output::Xv } else { gpu }
            }
        }
    }

    fn mpv_command(&mut self, command: Value) {
        if let Some(Session {
            mpv: Some((_, mpv)),
            ..
        }) = &mut self.session
        {
            mpv.command(command);
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Network(ssid) => {
                self.profile = self.config.profile_for_network(ssid.as_deref());
                self.wifi = ssid;
                self.network_known = true;
                if self.config.autostart && self.session.is_none() {
                    self.start();
                }
            }
            Event::Server(id, event) => {
                let Some(session) = self.session.as_mut().filter(|s| s.id == id) else {
                    return;
                };
                match event {
                    ServerEvent::WaitingForLive => self.status = Status::Offline,
                    ServerEvent::Qualities(list) => self.available = list,
                    ServerEvent::Ready(url) => {
                        session.url = Some(url);
                        self.launch_mpv();
                    }
                    ServerEvent::Opened(quality) => {
                        let current = session.mpv.as_ref().map(|(_, mpv)| mpv.output());
                        session.playing_quality = Some(quality.clone());
                        self.status = Status::Playing;
                        // Forecast failed: restart mpv with the suited output.
                        if current.is_some_and(|o| o != self.output_for(Some(&quality))) {
                            self.launch_mpv();
                        }
                    }
                    ServerEvent::Error(message) => session.last_error = Some(message),
                }
            }
            Event::Mpv(mpv_id, event) => {
                let Some(session) = &mut self.session else {
                    return;
                };
                let Some((_, mpv)) = session.mpv.as_mut().filter(|m| m.0 == mpv_id) else {
                    return;
                };
                match event {
                    MpvEvent::Connected(stream) => mpv.set_ipc(stream),
                    MpvEvent::Volume(volume) => {
                        let volume = volume.round().clamp(0.0, 100.0) as u8;
                        if volume != self.config.player.volume {
                            self.config.player.volume = volume;
                            self.config_dirty = true;
                        }
                    }
                    MpvEvent::Mute(muted) => self.muted = muted,
                    MpvEvent::EndFile(reason) => session.end_reason = Some(reason),
                }
            }
        }
    }

    /// Checks whether streamlink or mpv stopped by themselves.
    fn poll_processes(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        if session.server.exited().is_some() {
            let detail = session
                .last_error
                .clone()
                .unwrap_or_else(|| "arrêt inattendu".into());
            self.stop();
            self.status = Status::Error(format!("streamlink : {detail}"));
            return;
        }
        if !session.mpv.as_mut().is_some_and(|(_, mpv)| mpv.exited()) {
            return;
        }
        let reason = session.end_reason.take();
        session.mpv = None;

        if reason.as_deref() == Some("quit") {
            if self.detached {
                // Detached video window closed: attach the video back to the application.
                self.detached = false;
                self.launch_mpv();
            } else {
                self.stop();
            }
        } else if self.too_many_restarts() {
            self.stop();
            self.status =
                Status::Error("mpv s'arrête en boucle : vérifie le flux ou la qualité".into());
        } else {
            // End of the live or an interruption: restart everything, streamlink will wait for the next live.
            self.start();
        }
    }

    fn too_many_restarts(&mut self) -> bool {
        let now = Instant::now();
        self.restarts
            .retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        self.restarts.push(now);
        self.restarts.len() > 3
    }

    fn toggle_detach(&mut self) {
        if self.surface.is_none() {
            return;
        }
        if self.fullscreen {
            self.toggle_fullscreen();
        }
        self.detached = !self.detached;
        if self.session.as_ref().is_some_and(|s| s.mpv.is_some()) {
            self.launch_mpv();
        }
    }

    fn toggle_fullscreen(&mut self) {
        if self.detached {
            self.mpv_command(json!(["cycle", "fullscreen"]));
            return;
        }
        self.fullscreen = !self.fullscreen;
        self.apply_viewport_fullscreen();
    }

    /// F11: the window goes full screen (no title bar), the layout
    /// stays the same (with theatre mode: video + chat + controls).
    fn toggle_window_fullscreen(&mut self) {
        self.window_fullscreen = !self.window_fullscreen;
        self.apply_viewport_fullscreen();
    }

    fn apply_viewport_fullscreen(&self) {
        let fullscreen = self.window_fullscreen || (self.fullscreen && !self.detached);
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
    }

    fn toggle_pause(&mut self) {
        if self.paused {
            // Live, resuming where we stopped would accumulate lag:
            // we reconnect mpv to get back to the live, like the Twitch site does.
            self.launch_mpv();
        } else if self.session.as_ref().is_some_and(|s| s.mpv.is_some()) {
            self.mpv_command(json!(["set", "pause", "yes"]));
            self.paused = true;
        }
    }

    fn link_network(&mut self, ssid: String) {
        for profile in &mut self.config.profiles {
            profile.wifi_networks.retain(|n| *n != ssid);
        }
        self.config.profiles[self.profile].wifi_networks.push(ssid);
        self.config_dirty = true;
        self.save_config();
    }

    fn save_config(&mut self) {
        if self.config_error.is_some() || !self.config_dirty {
            return;
        }
        match self.config.save() {
            Ok(()) => self.config_dirty = false,
            Err(e) => self.status = Status::Error(e),
        }
    }

    // ---------- Interface ----------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // No shortcut while typing text (chat field, at step 2).
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let pressed = |key| ctx.input(|i| i.key_pressed(key));
        if pressed(Key::Space) {
            self.toggle_pause();
        }
        if pressed(Key::M) {
            self.mpv_command(json!(["cycle", "mute"]));
        }
        if pressed(Key::ArrowUp) {
            self.mpv_command(json!(["add", "volume", 5]));
        }
        if pressed(Key::ArrowDown) {
            self.mpv_command(json!(["add", "volume", -5]));
        }
        // Escape leaves the most "immersive" mode first.
        let escape = pressed(Key::Escape);
        if pressed(Key::F) || (escape && self.fullscreen) {
            self.toggle_fullscreen();
        } else if pressed(Key::F11) || (escape && self.window_fullscreen) {
            self.toggle_window_fullscreen();
        } else if pressed(Key::T) || (escape && self.theater) {
            self.theater = !self.theater;
        }
    }

    /// Top bar: channel, profile, state.
    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(style::icon(icon::BROADCAST).color(style::ACCENT));
            ui.label(RichText::new("Evolve TV").strong().color(style::ACCENT));
            ui.separator();
            ui.label(format!("Chaîne : {}", self.config.channel));
            ui.separator();

            let mut profile = self.profile;
            egui::ComboBox::from_id_salt("profil")
                .selected_text(format!("Profil : {}", self.config.profiles[profile].name))
                .show_ui(ui, |ui| {
                    for (i, p) in self.config.profiles.iter().enumerate() {
                        ui.selectable_value(
                            &mut profile,
                            i,
                            format!("{} (jusqu'à {})", p.name, p.max_quality),
                        );
                    }
                });
            if profile != self.profile {
                self.profile = profile;
                self.quality = None;
                if self.session.is_some() {
                    self.start();
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.status_ui(ui)
            });
        });
        ui.add_space(4.0);
    }

    fn quality_ui(&mut self, ui: &mut egui::Ui) {
        let auto = format!(
            "Auto (jusqu'à {})",
            self.config.profiles[self.profile].max_quality
        );
        let selected = self.quality.clone().unwrap_or_else(|| auto.clone());
        let button = ui
            .button(style::label_icon(&selected, icon::CARET_UP))
            .on_hover_text("Qualité de la vidéo");
        let offline = self.available.is_empty();
        let mut choice = None;
        // The bar being at the bottom of the window, the menu always opens upwards.
        egui::Popup::menu(&button)
            .align(egui::RectAlign::TOP_START)
            .show(|ui| {
                ui.set_min_width(180.0);
                if ui.selectable_label(self.quality.is_none(), &auto).clicked() {
                    choice = Some(None);
                }
                if offline {
                    ui.label(
                        RichText::new("Hors ligne : qualités habituelles")
                            .small()
                            .weak(),
                    );
                }
                let list: Vec<String> = if offline {
                    TYPICAL_QUALITIES.iter().map(|q| (*q).to_owned()).collect()
                } else {
                    self.available.clone()
                };
                for q in list {
                    let is_selected = self.quality.as_deref() == Some(q.as_str());
                    if ui.selectable_label(is_selected, &q).clicked() {
                        choice = Some(Some(q));
                    }
                }
            });
        if let Some(quality) = choice
            && quality != self.quality
        {
            self.quality = quality;
            if self.session.is_some() {
                self.start();
            }
        }
    }

    /// Controls bar under the video.
    fn player_bar_ui(&mut self, ui: &mut egui::Ui) {
        let big = |glyph: &str| style::icon(glyph).size(18.0);
        let playing = self.session.as_ref().is_some_and(|s| s.mpv.is_some());
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            if self.session.is_some() {
                if ui
                    .button(big(icon::STOP))
                    .on_hover_text("Arrêter le stream")
                    .clicked()
                {
                    self.stop();
                }
            } else if ui
                .button(big(icon::PLAY))
                .on_hover_text("Lancer le stream")
                .clicked()
            {
                self.start();
            }
            let (pause_icon, pause_tip) = if self.paused {
                (icon::PLAY, "Reprendre au direct (Espace)")
            } else {
                (icon::PAUSE, "Pause (Espace)")
            };
            if ui
                .add_enabled(playing, egui::Button::new(big(pause_icon)))
                .on_hover_text(pause_tip)
                .clicked()
            {
                self.toggle_pause();
            }

            let volume_icon = if self.muted || self.config.player.volume == 0 {
                icon::SPEAKER_X
            } else {
                icon::SPEAKER_HIGH
            };
            if ui
                .add_enabled(playing, egui::Button::new(big(volume_icon)))
                .on_hover_text("Couper / rétablir le son (M)")
                .clicked()
            {
                self.mpv_command(json!(["cycle", "mute"]));
            }
            let mut volume = self.config.player.volume;
            ui.spacing_mut().slider_width = 90.0;
            let slider = ui
                .add(egui::Slider::new(&mut volume, 0..=100).show_value(false))
                .on_hover_text(format!("Volume : {volume} % (flèches haut / bas)"));
            if slider.changed() {
                // Without a video running, the volume will be applied at the next launch.
                self.mpv_command(json!(["set_property", "volume", volume]));
                self.config.player.volume = volume;
                self.config_dirty = true;
            }
            ui.separator();
            self.quality_ui(ui);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (fullscreen_icon, fullscreen_tip) = if self.window_fullscreen {
                    (icon::CORNERS_IN, "Quitter le plein écran (F11 / Échap)")
                } else {
                    (
                        icon::CORNERS_OUT,
                        "Plein écran avec le chat (F11) · vidéo seule : F",
                    )
                };
                if ui
                    .button(big(fullscreen_icon))
                    .on_hover_text(fullscreen_tip)
                    .clicked()
                {
                    self.toggle_window_fullscreen();
                }
                let (detach_icon, detach_tip) = if self.detached {
                    (icon::ARROW_SQUARE_IN, "Rattacher la vidéo à la fenêtre")
                } else {
                    (
                        icon::ARROW_SQUARE_OUT,
                        "Détacher la vidéo dans sa propre fenêtre",
                    )
                };
                if ui
                    .add_enabled(self.surface.is_some(), egui::Button::new(big(detach_icon)))
                    .on_hover_text(detach_tip)
                    .on_disabled_hover_text(
                        "Vidéo intégrée indisponible (voir en bas de la fenêtre)",
                    )
                    .clicked()
                {
                    self.toggle_detach();
                }
                if ui
                    .add(egui::Button::new(big(icon::FRAME_CORNERS)).selected(self.theater))
                    .on_hover_text("Mode théâtre : masquer les barres (T)")
                    .clicked()
                {
                    self.theater = !self.theater;
                }
            });
        });
        ui.add_space(3.0);
    }

    fn status_ui(&self, ui: &mut egui::Ui) {
        match &self.status {
            Status::Stopped => ui.label(RichText::new("Arrêté").weak()),
            Status::Connecting => ui.label("Connexion à Twitch…"),
            Status::Offline => ui.colored_label(
                Color32::from_rgb(230, 180, 60),
                format!(
                    "{} est hors ligne : lancement automatique au début du live",
                    self.config.channel
                ),
            ),
            Status::Playing => {
                let quality = self
                    .session
                    .as_ref()
                    .and_then(|s| s.playing_quality.as_deref())
                    .unwrap_or("?");
                let output = match self.session.as_ref().and_then(|s| s.mpv.as_ref()) {
                    Some((_, mpv)) if mpv.output() == Output::Xv => " · Xv",
                    Some(_) => " · GPU",
                    None => "",
                };
                let pause = if self.paused { " · en pause" } else { "" };
                ui.colored_label(
                    Color32::from_rgb(235, 70, 70),
                    format!("En direct · {quality}{output}{pause}"),
                )
            }
            Status::Error(e) => {
                ui.colored_label(Color32::from_rgb(235, 70, 70), format!("Erreur : {e}"))
            }
        };
    }

    fn network_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match (&self.wifi, self.network_known) {
                (_, false) => ui.label("Réseau : détection…"),
                (None, true) => ui.label("Réseau : pas de Wi-Fi détecté"),
                (Some(ssid), true) => ui.label(format!("Wi-Fi : {ssid}")),
            };
            if let Some(ssid) = self.wifi.clone() {
                let profile = &self.config.profiles[self.profile];
                if profile.wifi_networks.contains(&ssid) {
                    ui.label(RichText::new(format!("(associé au profil {})", profile.name)).weak());
                } else if self.config_error.is_none()
                    && ui
                        .button(format!("Associer ce réseau au profil « {} »", profile.name))
                        .clicked()
                {
                    self.link_network(ssid);
                }
            }
            let warning = Color32::from_rgb(230, 180, 60);
            if let Some(e) = &self.config_error {
                ui.separator();
                ui.colored_label(warning, format!("⚠ Config par défaut utilisée : {e}"));
            }
            if let Some(e) = &self.embed_error {
                ui.separator();
                ui.colored_label(warning, format!("⚠ Vidéo intégrée indisponible : {e}"));
            }
        });
    }

    fn video_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
        let embedded = !self.detached && self.session.as_ref().is_some_and(|s| s.mpv.is_some());

        // egui cannot draw anything over mpv's X11 window: an open menu
        // (profile, quality…) would be hidden. The video is hidden during the choice (the sound continues).
        let menu_open = egui::Popup::is_any_open(ui.ctx());
        if let Some(surface) = &mut self.surface {
            let result = if embedded && !menu_open {
                // egui counts in "points", X11 in real pixels (different if the screen is zoomed).
                let ppp = ui.ctx().pixels_per_point();
                let px = |v: f32| (v * ppp).round();
                surface.show_at(
                    px(rect.min.x) as i32,
                    px(rect.min.y) as i32,
                    px(rect.width()) as u32,
                    px(rect.height()) as u32,
                )
            } else {
                surface.hide()
            };
            if let Err(e) = result {
                eprintln!("Fenêtre vidéo : {e}");
            }
        }

        if !embedded {
            let text = match &self.status {
                Status::Playing if self.detached => {
                    "Vidéo détachée dans sa propre fenêtre".to_owned()
                }
                Status::Stopped => {
                    "Clique sur le bouton lecture pour regarder le stream".to_owned()
                }
                Status::Connecting => "Connexion à Twitch…".to_owned(),
                Status::Offline => format!(
                    "{} est hors ligne\nLe stream se lancera tout seul au début du live",
                    self.config.channel
                ),
                Status::Playing => String::new(),
                Status::Error(e) => format!("Erreur : {e}"),
            };
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(18.0),
                Color32::GRAY,
            );
        }
    }
}

impl eframe::App for EvolveApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.rx.try_recv() {
            self.handle_event(event);
        }
        self.poll_processes();
        self.chat.logic();
        if let Some((saved_messages, favorites)) = self.chat.take_shortcuts_change() {
            self.config.saved_messages = saved_messages;
            self.config.favorite_emotes = favorites;
            self.config_dirty = true;
            self.save_config();
        }
        if self.session.is_some() {
            // Checks the state of the processes again twice a second.
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_shortcuts(&ui.ctx().clone());
        if !self.fullscreen {
            if !self.theater {
                egui::Panel::top("controles").show(ui, |ui| self.controls_ui(ui));
                egui::Panel::bottom("reseau").show(ui, |ui| self.network_ui(ui));
            }
            egui::Panel::right("chat")
                .resizable(true)
                .default_size(340.0)
                .min_size(220.0)
                .show(ui, |ui| self.chat.ui(ui));
            // Added after the chat: it only takes the width of the video.
            egui::Panel::bottom("barre_lecteur").show(ui, |ui| self.player_bar_ui(ui));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| self.video_ui(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.session = None;
        self.save_config();
    }
}

/// Creates the child X11 window that will host the video, inside the application's window.
fn create_surface(cc: &eframe::CreationContext<'_>) -> Result<VideoSurface, String> {
    let handle = cc.window_handle().map_err(|e| e.to_string())?;
    let parent = match handle.as_raw() {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).map_err(|e| e.to_string())?,
        RawWindowHandle::Xcb(h) => h.window.get(),
        _ => return Err("nécessite une session X11".into()),
    };
    VideoSurface::new(parent)
}

/// True if our own window is drawn by the NVIDIA OpenGL driver: the desktop
/// then runs on the NVIDIA, and mpv can limit itself to that driver. It is read from the
/// list of libraries loaded by our process (`/proc/self/maps`).
fn display_uses_nvidia() -> bool {
    Path::new(mpv::NVIDIA_EGL_VENDOR).exists()
        && fs::read_to_string("/proc/self/maps")
            .is_ok_and(|maps| maps.contains("libGLX_nvidia") || maps.contains("libEGL_nvidia"))
}
