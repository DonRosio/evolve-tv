//! Colonne de droite : connexion au compte Twitch + chat.
//!
//! Elle orchestre les threads de fond (vérification du jeton, connexion par code,
//! chat IRC, API Twitch) et reçoit leurs résultats par un canal, comme `app.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;

use crate::auth::{self, Outcome, Poll, Session};
use crate::browser;
use crate::chat::{Badge, Chat, ChatEvent, ChatLogin, SharedLogin};
use crate::chat_panel::ChatPanel;
use crate::config::{Config, SavedMessage};
use crate::emotes;
use crate::helix::{self, BadgeImage};
use crate::images::ImageCache;

/// Twitch demande de revérifier le jeton toutes les heures.
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Rechargement des emotes 7TV/BTTV/FFZ (une emote ajoutée par la chaîne apparaît ainsi).
const EMOTES_REFRESH: Duration = Duration::from_secs(30 * 60);
/// Pas de rechargement plus fréquent que ça (ex. reconnexions en série).
const EMOTES_MIN_INTERVAL: Duration = Duration::from_secs(60);

enum Msg {
    Auth(Outcome),
    DeviceCode { user_code: String, uri: String },
    DeviceFailed(String),
    Chat(u64, ChatEvent),
    Badges(Result<HashMap<Badge, BadgeImage>, String>),
    UserEmotes(Result<HashMap<String, String>, String>),
    ThirdParty(emotes::Loaded),
}

enum AuthState {
    /// Pas d'identifiant d'application dans la config : lecture seule.
    NoClientId,
    Checking,
    LoggedOut,
    Device {
        code: Option<(String, String)>,
        cancel: Arc<AtomicBool>,
    },
    LoggedIn(Session),
    Error(String),
}

/// Action demandée par un bouton (appliquée après le dessin, pour ne pas
/// modifier l'état pendant qu'on le lit).
enum Action {
    Login,
    CancelLogin,
    Logout,
    RefreshEmotes,
}

pub struct ChatColumn {
    ctx: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    channel: String,
    client_id: Option<String>,

    auth: AuthState,
    next_check: Instant,
    last_login: Instant,

    chat: Option<(u64, Chat)>,
    next_chat_id: u64,
    login: SharedLogin,
    room_id: Option<String>,
    badges_for: Option<String>,
    /// Dernier chargement des emotes tierces (chaîne + instant).
    emotes_loaded: Option<(String, Instant)>,

    panel: ChatPanel,
    images: ImageCache,
}

impl ChatColumn {
    pub fn new(ctx: &egui::Context, config: &Config) -> ChatColumn {
        let (tx, rx) = mpsc::channel();
        let client_id = config.twitch_client_id.clone();
        let mut panel = ChatPanel::new(config.animated_emotes);
        panel.set_shortcuts(
            config.saved_messages.clone(),
            config.favorite_emotes.clone(),
        );
        let mut column = ChatColumn {
            ctx: ctx.clone(),
            tx,
            rx,
            channel: config.channel.to_lowercase(),
            auth: if client_id.is_some() {
                AuthState::Checking
            } else {
                AuthState::NoClientId
            },
            client_id,
            next_check: Instant::now() + CHECK_INTERVAL,
            last_login: Instant::now(),
            chat: None,
            next_chat_id: 0,
            login: Arc::new(Mutex::new(None)),
            room_id: None,
            badges_for: None,
            emotes_loaded: None,
            panel,
            images: ImageCache::new(ctx),
        };
        match column.client_id.clone() {
            // Relit le trousseau ; le chat démarre quand on sait si on est connecté.
            Some(client_id) => column.spawn(move || Msg::Auth(auth::restore(&client_id))),
            None => column.restart_chat(),
        }
        column
    }

    /// Messages enregistrés et favorites modifiés depuis l'interface (à enregistrer).
    pub fn take_shortcuts_change(&mut self) -> Option<(Vec<SavedMessage>, Vec<String>)> {
        self.panel.take_shortcuts_change()
    }

    /// Lance `job` dans un thread et renvoie son résultat à l'interface.
    fn spawn(&self, job: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
    }

    fn restart_chat(&mut self) {
        self.next_chat_id += 1;
        let id = self.next_chat_id;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let chat = Chat::spawn(self.channel.clone(), self.login.clone(), move |event| {
            let _ = tx.send(Msg::Chat(id, event));
            ctx.request_repaint();
        });
        // Remplacer l'ancienne poignée arrête l'ancienne connexion.
        self.chat = Some((id, chat));
    }

    fn set_chat_login(&self, value: Option<ChatLogin>) {
        if let Ok(mut login) = self.login.lock() {
            *login = value;
        }
    }

    // ---------- Événements ----------

    /// À appeler à chaque image, avant le dessin.
    pub fn logic(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        self.images.poll();

        if Instant::now() >= self.next_check {
            self.next_check = Instant::now() + CHECK_INTERVAL;
            match (&self.auth, self.client_id.clone()) {
                (AuthState::LoggedIn(session), Some(client_id)) => {
                    let tokens = session.tokens.clone();
                    self.spawn(move || Msg::Auth(auth::check(&client_id, tokens)));
                }
                (AuthState::Error(_), Some(client_id)) => {
                    self.auth = AuthState::Checking;
                    self.spawn(move || Msg::Auth(auth::restore(&client_id)));
                }
                _ => {}
            }
        }
        self.fetch_badges_if_needed();
        if self
            .emotes_loaded
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= EMOTES_REFRESH)
        {
            self.load_emotes(false);
        }
    }

    /// Charge les emotes 7TV/BTTV/FFZ de la chaîne (sauf si c'est tout récent).
    fn load_emotes(&mut self, force: bool) {
        let Some(room) = self.room_id.clone() else {
            return;
        };
        let recent = self
            .emotes_loaded
            .as_ref()
            .is_some_and(|(loaded_room, at)| {
                *loaded_room == room && at.elapsed() < EMOTES_MIN_INTERVAL
            });
        if recent && !force {
            return;
        }
        self.emotes_loaded = Some((room.clone(), Instant::now()));
        self.spawn(move || Msg::ThirdParty(emotes::load(&room)));
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Auth(outcome) => self.on_auth(outcome),
            Msg::DeviceCode { user_code, uri } => {
                if let AuthState::Device { code, .. } = &mut self.auth {
                    *code = Some((user_code, uri));
                }
            }
            Msg::DeviceFailed(error) => {
                if matches!(self.auth, AuthState::Device { .. }) {
                    self.auth = AuthState::Error(error);
                }
            }
            Msg::Chat(id, event) => {
                if self
                    .chat
                    .as_ref()
                    .is_some_and(|(current, _)| *current == id)
                {
                    self.on_chat(event);
                }
            }
            Msg::Badges(result) => match result {
                Ok(badges) => self.panel.set_badges(badges),
                Err(e) => self
                    .panel
                    .push_system(format!("Badges indisponibles : {e}")),
            },
            Msg::ThirdParty(loaded) => {
                let counts: Vec<String> = loaded
                    .counts
                    .iter()
                    .map(|(provider, n)| format!("{} {n}", provider.label()))
                    .collect();
                self.panel
                    .push_system(format!("Emotes chargées : {}", counts.join(", ")));
                for error in loaded.errors {
                    self.panel.push_system(format!("⚠ Emotes {error}"));
                }
                self.panel.set_third_party(loaded.emotes);
            }
            Msg::UserEmotes(result) => match result {
                Ok(emotes) => self.panel.set_user_emotes(emotes),
                Err(e) => self
                    .panel
                    .push_system(format!("Tes emotes sont indisponibles : {e}")),
            },
        }
    }

    fn on_auth(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::LoggedIn(session) => {
                let identity = session.identity.clone();
                let same_account = matches!(&self.auth,
                    AuthState::LoggedIn(old) if old.identity.login == identity.login);
                self.set_chat_login(Some(ChatLogin {
                    login: identity.login.clone(),
                    access_token: session.tokens.access_token.clone(),
                }));
                self.next_check = Instant::now() + CHECK_INTERVAL;
                let token = session.tokens.access_token.clone();
                self.auth = AuthState::LoggedIn(session);
                if same_account {
                    return; // simple renouvellement du jeton : le chat continue
                }
                self.last_login = Instant::now();
                self.panel.set_me(Some(identity.login.clone()));
                self.badges_for = None;
                if let Some(client_id) = self.client_id.clone() {
                    let user_id = identity.user_id.clone();
                    self.spawn(move || {
                        Msg::UserEmotes(helix::user_emotes(&client_id, &token, &user_id))
                    });
                }
                self.restart_chat();
            }
            Outcome::LoggedOut => {
                if matches!(self.auth, AuthState::LoggedIn(_)) {
                    self.panel
                        .push_system("Session Twitch expirée : reconnecte-toi.");
                }
                self.set_logged_out();
            }
            Outcome::Failed(error) => {
                self.next_check = Instant::now() + RETRY_INTERVAL;
                // Connecté : on garde la session (souvent une coupure réseau passagère).
                if !matches!(self.auth, AuthState::LoggedIn(_)) {
                    self.auth = AuthState::Error(error);
                    if self.chat.is_none() {
                        self.restart_chat();
                    }
                }
            }
        }
    }

    fn set_logged_out(&mut self) {
        self.set_chat_login(None);
        self.auth = AuthState::LoggedOut;
        self.panel.set_me(None);
        self.restart_chat();
    }

    fn on_chat(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::Connected { logged_in } => {
                let mode = if logged_in { "" } else { " (lecture seule)" };
                eprintln!("Chat : connecté à #{}{mode}", self.channel);
                self.panel
                    .push_system(format!("Connecté au chat de {}{mode}", self.channel));
            }
            ChatEvent::Disconnected(reason) => {
                eprintln!("Chat : déconnecté ({reason})");
                self.panel
                    .push_system(format!("Chat déconnecté ({reason}), reconnexion…"));
            }
            ChatEvent::Message(message) => self.panel.push_message(message),
            ChatEvent::Notice(text) => self.panel.push_system(text),
            ChatEvent::SendRejected(reason) => self.panel.push_system(format!("⚠ {reason}")),
            ChatEvent::Clear(target) => self.panel.clear(target),
            ChatEvent::RoomId(id) => {
                self.room_id = Some(id);
                // À chaque (re)connexion au chat.
                self.load_emotes(false);
            }
            ChatEvent::AuthFailed => self.on_chat_auth_failed(),
        }
    }

    /// Twitch refuse notre jeton pour le chat : on tente un renouvellement, sauf si
    /// on vient d'en obtenir un (sinon on boucle) : retour en lecture seule.
    fn on_chat_auth_failed(&mut self) {
        let (AuthState::LoggedIn(session), Some(client_id)) = (&self.auth, self.client_id.clone())
        else {
            return;
        };
        if self.last_login.elapsed() < Duration::from_secs(60) {
            self.set_chat_login(None);
            self.auth = AuthState::Error("Twitch refuse l'accès au chat avec ce compte".into());
            self.panel.set_me(None);
            self.restart_chat();
            return;
        }
        self.last_login = Instant::now();
        let tokens = session.tokens.clone();
        self.spawn(move || Msg::Auth(auth::check(&client_id, tokens)));
    }

    fn fetch_badges_if_needed(&mut self) {
        let (Some(room), AuthState::LoggedIn(session), Some(client_id)) =
            (&self.room_id, &self.auth, &self.client_id)
        else {
            return;
        };
        if self.badges_for.as_ref() == Some(room) {
            return;
        }
        self.badges_for = Some(room.clone());
        let (room, token, client_id) = (
            room.clone(),
            session.tokens.access_token.clone(),
            client_id.clone(),
        );
        self.spawn(move || Msg::Badges(helix::badges(&client_id, &token, &room)));
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Login => {
                let Some(client_id) = self.client_id.clone() else {
                    return;
                };
                let cancel = Arc::new(AtomicBool::new(false));
                self.auth = AuthState::Device {
                    code: None,
                    cancel: cancel.clone(),
                };
                let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
                thread::spawn(move || {
                    let msg = device_flow(&client_id, &cancel, &tx, &ctx);
                    if !cancel.load(Ordering::Relaxed) {
                        let _ = tx.send(msg);
                        ctx.request_repaint();
                    }
                });
            }
            Action::CancelLogin => {
                if let AuthState::Device { cancel, .. } = &self.auth {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.auth = AuthState::LoggedOut;
            }
            Action::Logout => {
                if let (AuthState::LoggedIn(session), Some(client_id)) =
                    (&self.auth, self.client_id.clone())
                {
                    let token = session.tokens.access_token.clone();
                    thread::spawn(move || {
                        auth::revoke(&client_id, &token);
                        let _ = auth::delete_tokens();
                    });
                }
                self.panel.push_system("Déconnecté de Twitch.");
                self.set_logged_out();
            }
            Action::RefreshEmotes => self.load_emotes(true),
        }
    }

    // ---------- Interface ----------

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Chat de {}", self.channel)).heading());
        });
        let mut action = self.account_ui(ui);
        if ui
            .small_button(format!("{} Actualiser les emotes", icon::ARROW_CLOCKWISE))
            .on_hover_text("Recharge les emotes 7TV, BTTV et FFZ de la chaîne")
            .clicked()
        {
            action = Some(Action::RefreshEmotes);
        }
        ui.separator();
        if let Some(action) = action {
            self.apply(action);
        }

        let can_send = matches!(self.auth, AuthState::LoggedIn(_));
        if let Some(outgoing) = self.panel.ui(ui, &mut self.images, can_send)
            && let Some((_, chat)) = &self.chat
        {
            chat.send(&outgoing.text, outgoing.reply_to);
        }
    }

    fn account_ui(&self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        match &self.auth {
            AuthState::NoClientId => {
                ui.label(
                    RichText::new(
                        "Lecture seule : ajoute « twitch_client_id » dans la config pour écrire.",
                    )
                    .weak(),
                );
            }
            AuthState::Checking => {
                ui.label("Vérification du compte…");
            }
            AuthState::LoggedOut | AuthState::Error(_) => {
                if let AuthState::Error(e) = &self.auth {
                    ui.colored_label(Color32::from_rgb(235, 70, 70), format!("Compte : {e}"));
                }
                if ui
                    .button(format!("{} Se connecter à Twitch", icon::SIGN_IN))
                    .clicked()
                {
                    action = Some(Action::Login);
                }
            }
            AuthState::Device { code: None, .. } => {
                ui.label("Demande d'un code à Twitch…");
            }
            AuthState::Device {
                code: Some((user_code, uri)),
                ..
            } => {
                ui.label("Sur la page twitch.tv/activate (ouverte dans ton navigateur), saisis :");
                ui.label(RichText::new(user_code).monospace().size(24.0).strong());
                ui.horizontal(|ui| {
                    if ui.button("Copier le code").clicked() {
                        ui.ctx().copy_text(user_code.clone());
                    }
                    if ui.button("Rouvrir la page").clicked() {
                        open_in_browser(uri);
                    }
                    if ui.button("Annuler").clicked() {
                        action = Some(Action::CancelLogin);
                    }
                });
            }
            AuthState::LoggedIn(session) => {
                ui.horizontal(|ui| {
                    ui.label(format!("Connecté : {}", session.identity.login));
                    if ui
                        .small_button(format!("{} Se déconnecter", icon::SIGN_OUT))
                        .clicked()
                    {
                        action = Some(Action::Logout);
                    }
                });
            }
        }
        action
    }
}

/// Connexion par code (Device Code Flow), exécutée dans un thread.
fn device_flow(client_id: &str, cancel: &AtomicBool, tx: &Sender<Msg>, ctx: &egui::Context) -> Msg {
    let code = match auth::start_device_flow(client_id) {
        Ok(code) => code,
        Err(e) => return Msg::DeviceFailed(e),
    };
    let _ = tx.send(Msg::DeviceCode {
        user_code: code.user_code.clone(),
        uri: code.verification_uri.clone(),
    });
    ctx.request_repaint();
    open_in_browser(&code.verification_uri);

    let deadline = Instant::now() + Duration::from_secs(code.expires_in.min(30 * 60));
    let mut interval = Duration::from_secs(code.interval.clamp(1, 30));
    while Instant::now() < deadline {
        // Attente découpée en petits pas pour réagir vite à « Annuler ».
        let wake = Instant::now() + interval;
        while Instant::now() < wake {
            if cancel.load(Ordering::Relaxed) {
                return Msg::DeviceFailed("connexion annulée".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        match auth::poll_device_flow(client_id, &code.device_code) {
            Ok(Poll::Pending) => {}
            Ok(Poll::SlowDown) => interval += Duration::from_secs(5),
            Ok(Poll::Done(tokens)) => {
                return match auth::save_tokens(&tokens) {
                    Ok(()) => Msg::Auth(auth::check_fresh(client_id, tokens)),
                    Err(e) => Msg::DeviceFailed(e),
                };
            }
            Ok(Poll::Expired) => return Msg::DeviceFailed("code expiré ou refusé".into()),
            Err(e) => return Msg::DeviceFailed(e),
        }
    }
    Msg::DeviceFailed("code expiré".into())
}

/// Ouvre la page de validation de Twitch dans le navigateur par défaut.
fn open_in_browser(url: &str) {
    // Uniquement la page officielle de Twitch, jamais une adresse arbitraire.
    if !url.starts_with("https://www.twitch.tv/activate") {
        return;
    }
    browser::open(url);
}
