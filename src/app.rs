//! Fenêtre principale : contrôles en haut, vidéo au centre, chat à droite.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Key, RichText};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::{Value, json};

use crate::chat_column::ChatColumn;
use crate::config::{Config, VideoOutput};
use crate::mpv::{self, Mpv, MpvEvent, Output, Placement};
use crate::streamlink::{self, Server, ServerEvent};
use crate::video_surface::VideoSurface;
use crate::{network, paths};

/// Messages envoyés par les threads de fond vers l'interface.
///
/// Les threads ne touchent jamais directement à l'état de l'application (Rust
/// l'interdirait sans verrou) : ils envoient un message dans un canal, que
/// l'interface lit au début de chaque image.
enum Event {
    Network(Option<String>),
    /// Le numéro identifie la session : les messages d'un ancien streamlink sont ignorés.
    Server(u64, ServerEvent),
    /// Idem pour les instances de mpv.
    Mpv(u64, MpvEvent),
}

/// En sortie vidéo `auto`, hauteur maximale affichée par Xv (au-delà : GPU).
const XV_MAX_HEIGHT: u32 = 480;

enum Status {
    Stopped,
    Connecting,
    Offline,
    Playing,
    Error(String),
}

/// Un visionnage en cours : streamlink, puis mpv quand le flux est prêt.
struct Session {
    id: u64,
    // Les champs sont détruits dans l'ordre de déclaration : mpv d'abord, puis streamlink.
    mpv: Option<(u64, Mpv)>,
    server: Server,
    /// Liste de qualités transmise à streamlink (la première disponible est jouée).
    qualities: Vec<String>,
    url: Option<String>,
    playing_quality: Option<String>,
    last_error: Option<String>,
    /// Raison de la dernière fin de lecture signalée par mpv.
    end_reason: Option<String>,
}

pub struct EvolveApp {
    ctx: egui::Context,
    tx: Sender<Event>,
    rx: Receiver<Event>,

    config: Config,
    /// Erreur de lecture de la config : on utilise alors les valeurs par défaut
    /// sans jamais réécrire le fichier, pour ne pas effacer tes modifications.
    config_error: Option<String>,
    config_dirty: bool,
    streamlink_program: Option<PathBuf>,
    mpv_program: Option<PathBuf>,
    /// Le bureau est affiché par le pilote NVIDIA (voir `display_uses_nvidia`).
    nvidia_display: bool,

    profile: usize,
    /// Qualité choisie à la main (`None` : automatique selon le profil).
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
    fullscreen: bool,
    paused: bool,

    chat: ChatColumn,
}

impl EvolveApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        config_error: Option<String>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        install_fallback_font(&cc.egui_ctx);

        let (surface, embed_error) = match create_surface(cc) {
            Ok(surface) => (Some(surface), None),
            Err(e) => (None, Some(e)),
        };
        let streamlink_program =
            paths::find_program("streamlink", config.player.streamlink_path.as_deref());
        let mpv_program = paths::find_program("mpv", config.player.mpv_path.as_deref());

        // Détection du Wi-Fi en arrière-plan ; le stream démarre quand elle répond.
        let (tx_net, ctx) = (tx.clone(), cc.egui_ctx.clone());
        thread::spawn(move || {
            let _ = tx_net.send(Event::Network(network::current_wifi()));
            ctx.request_repaint();
        });

        let chat = ChatColumn::new(
            &cc.egui_ctx,
            &config.channel,
            config.twitch_client_id.clone(),
            config.animated_emotes,
        );

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
            paused: false,
        }
    }

    // ---------- Gestion du visionnage ----------

    /// (Re)lance streamlink avec le profil et la qualité actuels.
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
        // Détruire la session arrête mpv et streamlink (voir leurs `impl Drop`).
        self.session = None;
        self.status = Status::Stopped;
        self.paused = false;
    }

    /// Lance (ou relance) mpv sur le flux de la session, intégré ou détaché.
    fn launch_mpv(&mut self) {
        // Qualité jouée si streamlink l'a déjà annoncée, sinon celle qu'il va choisir.
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
        session.mpv = None; // ferme l'ancien mpv s'il y en a un
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

    /// Sortie vidéo adaptée à une qualité donnée.
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
                        // Prévision ratée : on relance mpv avec la sortie adaptée.
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
                    MpvEvent::EndFile(reason) => session.end_reason = Some(reason),
                }
            }
        }
    }

    /// Vérifie si streamlink ou mpv se sont arrêtés d'eux-mêmes.
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
                // Fenêtre vidéo détachée fermée : on rattache la vidéo à l'application.
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
            // Fin du live ou coupure : on relance tout, streamlink attendra le prochain live.
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
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    fn toggle_pause(&mut self) {
        if self.paused {
            // En direct, reprendre là où on s'était arrêté accumulerait du retard :
            // on reconnecte mpv pour revenir au direct, comme le site Twitch.
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
        // Pas de raccourci pendant la saisie de texte (champ du chat, à l'étape 2).
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
        if pressed(Key::F11) || pressed(Key::F) || (pressed(Key::Escape) && self.fullscreen) {
            self.toggle_fullscreen();
        }
    }

    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Evolve TV").strong());
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

            let auto = format!(
                "Auto (jusqu'à {})",
                self.config.profiles[self.profile].max_quality
            );
            let mut quality = self.quality.clone();
            egui::ComboBox::from_id_salt("qualite")
                .selected_text(quality.clone().unwrap_or_else(|| auto.clone()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut quality, None, auto);
                    for q in &self.available {
                        ui.selectable_value(&mut quality, Some(q.clone()), q.as_str());
                    }
                });
            if quality != self.quality {
                self.quality = quality;
                if self.session.is_some() {
                    self.start();
                }
            }
            ui.separator();

            if self.session.is_some() {
                if ui.button("⏹ Arrêter").clicked() {
                    self.stop();
                }
            } else if ui.button("▶ Lancer").clicked() {
                self.start();
            }
            let label = if self.detached {
                "Rattacher la vidéo"
            } else {
                "Détacher la vidéo"
            };
            let detach = ui
                .add_enabled(self.surface.is_some(), egui::Button::new(label))
                .on_disabled_hover_text("Vidéo intégrée indisponible (voir en bas de la fenêtre)");
            if detach.clicked() {
                self.toggle_detach();
            }
            if ui.button("Plein écran (F)").clicked() {
                self.toggle_fullscreen();
            }
            ui.separator();
            self.status_ui(ui);
        });
        ui.add_space(4.0);
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

        // egui ne peut rien dessiner par-dessus la fenêtre X11 de mpv : un menu ouvert
        // (profil, qualité…) serait caché. On masque la vidéo le temps du choix (le son continue).
        let menu_open = egui::Popup::is_any_open(ui.ctx());
        if let Some(surface) = &mut self.surface {
            let result = if embedded && !menu_open {
                // egui compte en « points », X11 en pixels réels (différents si l'écran est zoomé).
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
                Status::Stopped => "Clique sur « Lancer » pour regarder le stream".to_owned(),
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
        if self.session.is_some() {
            // Revérifie l'état des processus deux fois par seconde.
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_shortcuts(&ui.ctx().clone());
        if !self.fullscreen {
            egui::Panel::top("controles").show(ui, |ui| self.controls_ui(ui));
            egui::Panel::bottom("reseau").show(ui, |ui| self.network_ui(ui));
            egui::Panel::right("chat")
                .resizable(true)
                .default_size(340.0)
                .min_size(220.0)
                .show(ui, |ui| self.chat.ui(ui));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| self.video_ui(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.session = None;
        self.save_config();
    }
}

/// Police système utilisée en secours pour les caractères absents de la police
/// d'egui : braille (art ASCII du chat), flèches, symboles…
const FALLBACK_FONT: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";

fn install_fallback_font(ctx: &egui::Context) {
    // Taille plafonnée : on ne charge pas en mémoire un fichier anormal.
    let bytes = match fs::metadata(FALLBACK_FONT) {
        Ok(meta) if meta.len() <= 4 * 1024 * 1024 => fs::read(FALLBACK_FONT).ok(),
        _ => None,
    };
    let Some(bytes) = bytes else {
        eprintln!("Police de secours absente ({FALLBACK_FONT}) : l'art ASCII peut mal s'afficher.");
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "dejavu".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    // En dernier : elle ne sert que si les autres polices n'ont pas le caractère.
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("dejavu".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Crée la fenêtre X11 enfant qui accueillera la vidéo, dans la fenêtre de l'application.
fn create_surface(cc: &eframe::CreationContext<'_>) -> Result<VideoSurface, String> {
    let handle = cc.window_handle().map_err(|e| e.to_string())?;
    let parent = match handle.as_raw() {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).map_err(|e| e.to_string())?,
        RawWindowHandle::Xcb(h) => h.window.get(),
        _ => return Err("nécessite une session X11".into()),
    };
    VideoSurface::new(parent)
}

/// Vrai si notre propre fenêtre est dessinée par le pilote OpenGL NVIDIA : le bureau
/// tourne alors sur la NVIDIA, et mpv peut se limiter à ce pilote. On le lit dans la
/// liste des bibliothèques chargées par notre processus (`/proc/self/maps`).
fn display_uses_nvidia() -> bool {
    Path::new(mpv::NVIDIA_EGL_VENDOR).exists()
        && fs::read_to_string("/proc/self/maps")
            .is_ok_and(|maps| maps.contains("libGLX_nvidia") || maps.contains("libEGL_nvidia"))
}
