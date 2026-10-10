//! Panneau du chat : messages avec badges et emotes (animées, zero-width),
//! défilement, zone de saisie avec autocomplétion (Tab).

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{self, Color32, Key, Modifiers, Rect, RichText, Sense, Vec2};

use crate::browser;
use crate::chat::{Badge, ChatMessage, ClearTarget};
use crate::emoji::{self, Piece};
use crate::emotes::{Emote, EmoteMap, Provider};
use crate::helix::BadgeImage;
use crate::images::{ImageCache, Picture};

/// Messages gardés en mémoire (les plus anciens sont oubliés).
const MAX_LINES: usize = 300;
const MAX_CHATTERS: usize = 300;
const EMOTE_HEIGHT: f32 = 28.0;
const BADGE_SIZE: f32 = 18.0;
/// Les emojis sont affichés à la taille du texte.
const EMOJI_SIZE: f32 = 18.0;
const SUGGESTION_HEIGHT: f32 = 20.0;
/// Couleur des liens (violet, comme sur Twitch).
const LINK_COLOR: Color32 = Color32::from_rgb(169, 112, 255);
/// Au-delà de cette longueur, un mot peut être coupé en fin de ligne.
const LONG_WORD: usize = 30;
/// Les animations sont redessinées au plus 30 fois par seconde.
const MIN_FRAME_DELAY: f64 = 1.0 / 30.0;

/// Couleurs attribuées par Twitch aux comptes qui n'en ont pas choisi.
const DEFAULT_COLORS: [[u8; 3]; 15] = [
    [255, 0, 0],
    [0, 0, 255],
    [0, 128, 0],
    [178, 34, 34],
    [255, 127, 80],
    [154, 205, 50],
    [255, 69, 0],
    [46, 139, 87],
    [218, 165, 32],
    [210, 105, 30],
    [95, 158, 160],
    [30, 144, 255],
    [255, 105, 180],
    [138, 43, 226],
    [0, 255, 127],
];

#[derive(Debug, PartialEq)]
enum Segment {
    Text(String),
    Link(String),
    /// Emoji affiché en couleur (image Twemoji).
    Emoji {
        text: String,
        url: String,
    },
    Emote {
        name: String,
        url: String,
        zero_width: bool,
        provider: Provider,
    },
}

enum LineKind {
    Chat {
        name: String,
        color: Color32,
        badges: Vec<Badge>,
        segments: Vec<Segment>,
        action: bool,
        mention: bool,
    },
    System(String),
}

struct Line {
    id: String,
    login: String,
    kind: LineKind,
    deleted: bool,
    /// Hauteur mesurée au dernier affichage, et largeur correspondante : permet de
    /// ne dessiner que les lignes visibles (les autres sont remplacées par un espace vide).
    /// `Cell` autorise la mise à jour pendant qu'on parcourt les lignes en lecture.
    height: Cell<f32>,
    width: Cell<f32>,
}

/// Autocomplétion en cours (Tab / Shift+Tab font défiler les propositions).
struct Completion {
    /// Position (en octets) du début du mot complété dans la saisie.
    start: usize,
    candidates: Vec<String>,
    index: usize,
    /// Texte de la saisie juste après notre complétion : s'il change, on repart de zéro.
    applied: String,
}

pub struct ChatPanel {
    lines: VecDeque<Line>,
    input: String,
    my_login: Option<String>,
    /// Emotes Twitch utilisables par moi (pour reconnaître celles de mes messages).
    user_emotes: EmoteMap,
    /// Emotes 7TV, BTTV et FFZ.
    third_party: EmoteMap,
    badges: HashMap<Badge, BadgeImage>,
    /// Pseudos récemment actifs, le plus récent à la fin.
    chatters: VecDeque<String>,
    completion: Option<Completion>,
    /// Lien cliqué, en attente de confirmation avant ouverture.
    pending_link: Option<String>,
    scroll_to_bottom: bool,
    animate: bool,
}

/// Ce que le dessin des lignes doit signaler au panneau.
#[derive(Default)]
struct DrawOutput {
    /// Délai avant la prochaine frame d'une emote animée visible.
    next_repaint: Option<f64>,
    clicked_link: Option<String>,
}

impl ChatPanel {
    pub fn new(animate: bool) -> ChatPanel {
        ChatPanel {
            lines: VecDeque::new(),
            input: String::new(),
            my_login: None,
            user_emotes: EmoteMap::new(),
            third_party: EmoteMap::new(),
            badges: HashMap::new(),
            chatters: VecDeque::new(),
            completion: None,
            pending_link: None,
            scroll_to_bottom: false,
            animate,
        }
    }

    pub fn set_me(&mut self, login: Option<String>) {
        if login.is_none() {
            self.user_emotes.clear();
        }
        self.my_login = login;
    }

    /// Emotes Twitch de l'utilisateur : nom → identifiant.
    pub fn set_user_emotes(&mut self, emotes: HashMap<String, String>) {
        self.user_emotes = emotes
            .into_iter()
            .filter(|(_, id)| is_valid_emote_id(id))
            .map(|(name, id)| {
                let emote = Emote {
                    name: name.clone(),
                    url: twitch_emote_url(&id),
                    zero_width: false,
                    provider: Provider::Twitch,
                };
                (name, emote)
            })
            .collect();
    }

    pub fn set_third_party(&mut self, emotes: EmoteMap) {
        self.third_party = emotes;
    }

    pub fn set_badges(&mut self, badges: HashMap<Badge, BadgeImage>) {
        self.badges = badges;
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        self.push(String::new(), String::new(), LineKind::System(text.into()));
    }

    pub fn push_message(&mut self, message: ChatMessage) {
        let own = message.own;
        // Les emotes 7TV/BTTV/FFZ se reconnaissent par leur nom dans tous les messages.
        // Twitch ne renvoie pas nos propres messages, donc ni la position de leurs
        // emotes Twitch : on les reconnaît aussi par leur nom.
        let lookup = |word: &str| {
            let twitch = own.then(|| self.user_emotes.get(word)).flatten();
            twitch.or_else(|| self.third_party.get(word)).cloned()
        };
        let segments = build_segments(&message.text, &message.emotes, &lookup);
        let mention = !own
            && self
                .my_login
                .as_deref()
                .is_some_and(|me| mentions(&message.text, me));
        if !own {
            self.remember_chatter(&message.display_name);
        }
        let kind = LineKind::Chat {
            name: message.display_name,
            color: readable_color(message.color, &message.login),
            badges: message.badges,
            segments,
            action: message.action,
            mention,
        };
        self.push(message.id, message.login, kind);
    }

    fn remember_chatter(&mut self, name: &str) {
        self.chatters.retain(|n| n != name);
        self.chatters.push_back(name.to_owned());
        if self.chatters.len() > MAX_CHATTERS {
            self.chatters.pop_front();
        }
    }

    pub fn clear(&mut self, target: ClearTarget) {
        let matches: Box<dyn Fn(&Line) -> bool> = match &target {
            ClearTarget::Message(id) => Box::new(move |l: &Line| l.id == *id),
            ClearTarget::User(login) => Box::new(move |l: &Line| l.login == *login),
            ClearTarget::All => {
                self.push_system("Le chat a été effacé par un modérateur.");
                return;
            }
        };
        for line in self.lines.iter_mut().filter(|l| matches(l)) {
            line.deleted = true;
            line.height.set(0.0); // à remesurer
        }
    }

    fn push(&mut self, id: String, login: String, kind: LineKind) {
        self.lines.push_back(Line {
            id,
            login,
            kind,
            deleted: false,
            height: Cell::new(0.0),
            width: Cell::new(0.0),
        });
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    // ---------- Autocomplétion ----------

    fn emote_named(&self, name: &str) -> Option<&Emote> {
        self.user_emotes
            .get(name)
            .or_else(|| self.third_party.get(name))
    }

    /// Propositions pour `word` : emotes puis pseudos (ou seulement les pseudos avec `@`).
    fn candidates(&self, word: &str) -> Vec<String> {
        let lower = word.to_lowercase();
        let chatters = |prefix: &str| -> Vec<String> {
            self.chatters
                .iter()
                .rev()
                .filter(|name| name.to_lowercase().starts_with(prefix))
                .cloned()
                .collect()
        };
        if let Some(prefix) = lower.strip_prefix('@') {
            return chatters(prefix)
                .into_iter()
                .map(|n| format!("@{n}"))
                .take(100)
                .collect();
        }
        let mut names: Vec<&String> = self
            .third_party
            .keys()
            .chain(self.user_emotes.keys())
            .filter(|name| name.to_lowercase().starts_with(&lower))
            .collect();
        names.sort_by_cached_key(|name| (name.to_lowercase(), name.as_str()));
        names.dedup();
        let mut out: Vec<String> = names.into_iter().take(100).cloned().collect();
        for name in chatters(&lower).into_iter().take(20) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// Complète le dernier mot de la saisie. Renvoie `true` si la saisie a changé.
    fn complete(&mut self, backwards: bool) -> bool {
        if let Some(completion) = &mut self.completion
            && completion.applied == self.input
        {
            let n = completion.candidates.len();
            completion.index = if backwards {
                (completion.index + n - 1) % n
            } else {
                (completion.index + 1) % n
            };
            let candidate = &completion.candidates[completion.index];
            completion.applied = format!("{}{candidate} ", &self.input[..completion.start]);
            self.input = completion.applied.clone();
            return true;
        }
        let start = self.input.rfind(' ').map_or(0, |i| i + 1);
        let word = &self.input[start..];
        if word.is_empty() {
            return false;
        }
        let candidates = self.candidates(word);
        if candidates.is_empty() {
            return false;
        }
        let index = if backwards { candidates.len() - 1 } else { 0 };
        let applied = format!("{}{} ", &self.input[..start], candidates[index]);
        self.input = applied.clone();
        self.completion = Some(Completion {
            start,
            candidates,
            index,
            applied,
        });
        true
    }

    // ---------- Affichage ----------

    /// Dessine le panneau. Renvoie le message à envoyer quand on appuie sur Entrée.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        images: &mut ImageCache,
        can_send: bool,
    ) -> Option<String> {
        let time = ui.input(|i| i.time);
        let mut out = DrawOutput::default();
        let mut to_send = None;

        egui::Panel::bottom("chat_saisie").show(ui, |ui| {
            ui.add_space(4.0);
            self.link_confirmation_ui(ui);
            self.suggestions_ui(ui, images, time);
            let id = egui::Id::new("chat_saisie_texte");
            if can_send && ui.memory(|m| m.has_focus(id)) {
                // Tab est intercepté avant le champ de saisie, qui ne le voit donc pas.
                let back = ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Tab));
                let forward = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab));
                if (forward || back) && self.complete(back) {
                    move_cursor_to_end(ui.ctx(), id, &self.input);
                }
            }
            let hint = if can_send {
                "Envoyer un message (Tab : compléter)"
            } else {
                "Connecte-toi pour écrire"
            };
            let edit = egui::TextEdit::singleline(&mut self.input)
                .id(id)
                // Tab ne fait pas quitter le champ (il sert à l'autocomplétion).
                .lock_focus(true)
                .hint_text(hint)
                .char_limit(500)
                .desired_width(f32::INFINITY);
            let response = ui.add_enabled(can_send, edit);
            if self
                .completion
                .as_ref()
                .is_some_and(|c| c.applied != self.input)
            {
                self.completion = None;
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                if !self.input.trim().is_empty() {
                    to_send = Some(std::mem::take(&mut self.input));
                    self.completion = None;
                    self.scroll_to_bottom = true;
                }
                response.request_focus();
            }
            ui.add_space(4.0);
        });

        let scroll_to_bottom = std::mem::take(&mut self.scroll_to_bottom);
        let area = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            // Reste collé en bas… sauf si on remonte dans l'historique.
            .stick_to_bottom(true);
        let output = area.show_viewport(ui, |ui, viewport| {
            ui.spacing_mut().item_spacing.y = 3.0;
            let spacing = ui.spacing().item_spacing.y;
            let width = ui.available_width();
            let margin = 100.0;
            let mut y = 0.0;
            for line in &self.lines {
                let height = line.height.get();
                let measured = height > 0.0 && line.width.get() == width;
                let visible = y + height >= viewport.min.y - margin && y <= viewport.max.y + margin;
                if measured && !visible {
                    // Ligne hors écran : un simple espace de la bonne hauteur.
                    ui.allocate_space(egui::vec2(width, height));
                } else {
                    let rect = self.line_ui(ui, line, images, time, &mut out);
                    line.height.set(rect.height());
                    line.width.set(width);
                }
                y += line.height.get() + spacing;
            }
            if scroll_to_bottom {
                // Fait défiler jusqu'à la fin réelle du contenu (le défilement se
                // « recolle » alors au bas du chat).
                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
            }
        });
        if let Some(url) = out.clicked_link.take() {
            self.pending_link = Some(url);
        }

        // Redessine juste à temps pour la prochaine frame des emotes animées visibles.
        if let Some(delay) = out.next_repaint {
            let delay = delay.max(MIN_FRAME_DELAY);
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(delay));
        }

        let at_bottom =
            output.state.offset.y + output.inner_rect.height() >= output.content_size.y - 4.0;
        if !at_bottom {
            let rect = Rect::from_center_size(
                output.inner_rect.center_bottom() - egui::vec2(0.0, 18.0),
                egui::vec2(240.0, 26.0),
            );
            if ui
                .put(rect, egui::Button::new("Défilement en pause : reprendre"))
                .clicked()
            {
                self.scroll_to_bottom = true;
            }
        }
        to_send
    }

    /// Barre « Ouvrir ce lien ? » : montre le vrai domaine avant d'ouvrir le navigateur.
    fn link_confirmation_ui(&mut self, ui: &mut egui::Ui) {
        let Some(url) = self.pending_link.clone() else {
            return;
        };
        let mut close = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Ouvrir un lien vers");
                ui.label(
                    RichText::new(browser::domain(&url))
                        .strong()
                        .color(LINK_COLOR),
                );
                ui.label("?");
            });
            let shown: String = url.chars().take(120).collect();
            ui.label(RichText::new(shown).small().weak());
            ui.horizontal(|ui| {
                if ui.button("Ouvrir").clicked() {
                    browser::open(&url);
                    close = true;
                }
                if ui.button("Copier").clicked() {
                    ui.ctx().copy_text(url.clone());
                    close = true;
                }
                if ui.button("Annuler").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.pending_link = None;
        }
    }

    /// Bande des propositions d'autocomplétion, au-dessus de la saisie.
    fn suggestions_ui(&self, ui: &mut egui::Ui, images: &mut ImageCache, time: f64) {
        let Some(completion) = self.completion.as_ref().filter(|c| c.applied == self.input) else {
            return;
        };
        ui.horizontal(|ui| {
            let first = completion.index.saturating_sub(3);
            for (i, name) in completion.candidates.iter().enumerate().skip(first).take(8) {
                if let Some(emote) = self.emote_named(name)
                    && let Some(picture) = images.get(&emote.url)
                {
                    let frame = picture.frame_at(time, false);
                    let size = frame.size * (SUGGESTION_HEIGHT / frame.size.y.max(1.0));
                    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                    ui.painter()
                        .image(frame.texture, rect, frame.uv, Color32::WHITE);
                }
                let text = RichText::new(name);
                ui.label(if i == completion.index {
                    text.strong()
                        .color(Color32::WHITE)
                        .background_color(Color32::from_rgb(75, 60, 130))
                } else {
                    text.weak()
                });
                ui.add_space(6.0);
            }
            ui.label(
                RichText::new(format!(
                    "{}/{}",
                    completion.index + 1,
                    completion.candidates.len()
                ))
                .weak(),
            );
        });
    }

    /// Dessine une ligne et renvoie le rectangle occupé.
    fn line_ui(
        &self,
        ui: &mut egui::Ui,
        line: &Line,
        images: &mut ImageCache,
        time: f64,
        out: &mut DrawOutput,
    ) -> Rect {
        let (name, color, badges, segments, action, mention) = match &line.kind {
            LineKind::System(text) => {
                return ui
                    .label(RichText::new(text).italics().color(Color32::GRAY))
                    .rect;
            }
            LineKind::Chat {
                name,
                color,
                badges,
                segments,
                action,
                mention,
            } => (name, *color, badges, segments, *action, *mention),
        };

        let mut frame = egui::Frame::new().inner_margin(2.0);
        if mention {
            frame = frame.fill(Color32::from_rgba_unmultiplied(220, 50, 50, 45));
        }
        let response = frame.show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for badge in badges {
                    let Some(info) = self.badges.get(badge) else {
                        continue;
                    };
                    if let Some(picture) = images.get(&info.url) {
                        let frame = picture.frame_at(time, false);
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::splat(BADGE_SIZE), Sense::hover());
                        ui.painter()
                            .image(frame.texture, rect, frame.uv, Color32::WHITE);
                        response.on_hover_text(&info.title);
                        ui.add_space(3.0);
                    }
                }
                ui.label(RichText::new(name).color(color).strong());
                ui.label(if action { " " } else { ": " });
                if line.deleted {
                    ui.label(RichText::new("<message supprimé>").italics().weak());
                    return;
                }
                // Dernière emote dessinée : une emote zero-width se pose par-dessus.
                let mut last_emote: Option<Rect> = None;
                for segment in segments {
                    match segment {
                        Segment::Text(text) => {
                            if !text.trim().is_empty() {
                                last_emote = None;
                            }
                            // Un mot par étiquette : le retour à la ligne se fait entre
                            // les mots, comme sur Twitch (seuls les mots très longs sont coupés).
                            for word in text.split_inclusive(' ') {
                                let mut rich = RichText::new(word);
                                if action {
                                    rich = rich.color(color).italics();
                                }
                                ui.add(egui::Label::new(rich).wrap_mode(wrap_mode(word)));
                            }
                        }
                        Segment::Emoji { text, url } => {
                            last_emote = None;
                            match images.get(url) {
                                Some(picture) => {
                                    let frame = picture.frame_at(time, false);
                                    let (rect, _) = ui.allocate_exact_size(
                                        Vec2::splat(EMOJI_SIZE),
                                        Sense::hover(),
                                    );
                                    if ui.is_rect_visible(rect) {
                                        ui.painter().image(
                                            frame.texture,
                                            rect,
                                            frame.uv,
                                            Color32::WHITE,
                                        );
                                    }
                                }
                                // En chargement, ou inconnu de Twemoji : le caractère en texte.
                                None => {
                                    ui.add(
                                        egui::Label::new(text.as_str())
                                            .wrap_mode(egui::TextWrapMode::Extend),
                                    );
                                }
                            }
                        }
                        Segment::Link(url) => {
                            last_emote = None;
                            let rich = RichText::new(url).color(LINK_COLOR).underline();
                            let response = ui
                                .add(
                                    egui::Label::new(rich)
                                        .wrap_mode(wrap_mode(url))
                                        .sense(Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(format!("Ouvrir {}", browser::domain(url)));
                            if response.clicked() {
                                out.clicked_link = Some(url.clone());
                            }
                        }
                        Segment::Emote {
                            name,
                            url,
                            zero_width,
                            provider,
                        } => match images.get(url) {
                            Some(picture) => {
                                let base = last_emote.filter(|_| *zero_width);
                                let rect = self.paint_emote(ui, picture, base, time, out);
                                if base.is_none() {
                                    ui.interact(
                                        rect,
                                        ui.id().with(rect.min.x as i32),
                                        Sense::hover(),
                                    )
                                    .on_hover_text(format!("{name} · {}", provider.label()));
                                    last_emote = Some(rect);
                                }
                            }
                            // Pas encore chargée : son nom en attendant.
                            None => {
                                ui.label(RichText::new(name).weak());
                                last_emote = None;
                            }
                        },
                    }
                }
            });
        });
        response.response.rect
    }

    /// Dessine une emote (la frame du moment si elle est animée). Avec `overlay_on`,
    /// elle est centrée sur cette zone (emote zero-width) sans prendre de place.
    fn paint_emote(
        &self,
        ui: &mut egui::Ui,
        picture: &Picture,
        overlay_on: Option<Rect>,
        time: f64,
        out: &mut DrawOutput,
    ) -> Rect {
        let frame = picture.frame_at(time, self.animate);
        let size = frame.size * (EMOTE_HEIGHT / frame.size.y.max(1.0));
        let rect = match overlay_on {
            Some(base) => Rect::from_center_size(base.center(), size),
            None => ui.allocate_exact_size(size, Sense::hover()).0,
        };
        // Seules les emotes visibles à l'écran sont animées.
        if ui.is_rect_visible(rect) {
            ui.painter()
                .image(frame.texture, rect, frame.uv, Color32::WHITE);
            if let Some(delay) = frame.next_change {
                out.next_repaint = Some(out.next_repaint.map_or(delay, |d| d.min(delay)));
            }
        }
        rect
    }
}

/// Mot court : jamais coupé (il passe à la ligne entier). Mot très long (lien…) :
/// coupé où il faut pour rester entièrement visible.
fn wrap_mode(word: &str) -> egui::TextWrapMode {
    if word.chars().count() > LONG_WORD {
        egui::TextWrapMode::Wrap
    } else {
        egui::TextWrapMode::Extend
    }
}

/// Place le curseur de la zone de saisie à la fin du texte.
fn move_cursor_to_end(ctx: &egui::Context, id: egui::Id, text: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let end = CCursor::new(text.chars().count());
        state.cursor.set_char_range(Some(CCursorRange::one(end)));
        state.store(ctx, id);
    }
}

/// Les identifiants d'emotes Twitch ne contiennent que lettres, chiffres et `_`.
fn is_valid_emote_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `default` : GIF si l'emote est animée, PNG sinon.
fn twitch_emote_url(id: &str) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/1.0")
}

/// Découpe le texte en morceaux de texte et d'emotes.
/// `positions` : emotes Twitch avec leur position (en caractères), fournies par Twitch.
/// `lookup` : emotes à reconnaître par leur nom (mot entier).
fn build_segments(
    text: &str,
    positions: &[(String, usize, usize)],
    lookup: &dyn Fn(&str) -> Option<Emote>,
) -> Vec<Segment> {
    let chars: Vec<char> = text.chars().collect();
    let mut segments = Vec::new();
    let mut pos = 0;
    for (id, start, end) in positions {
        // Positions incohérentes (chevauchement, hors du texte) : ignorées.
        if *start < pos || *end >= chars.len() || !is_valid_emote_id(id) {
            continue;
        }
        push_words(
            &mut segments,
            &chars[pos..*start].iter().collect::<String>(),
            lookup,
        );
        segments.push(Segment::Emote {
            name: chars[*start..=*end].iter().collect(),
            url: twitch_emote_url(id),
            zero_width: false,
            provider: Provider::Twitch,
        });
        pos = end + 1;
    }
    push_words(
        &mut segments,
        &chars[pos..].iter().collect::<String>(),
        lookup,
    );
    segments
}

fn push_words(segments: &mut Vec<Segment>, text: &str, lookup: &dyn Fn(&str) -> Option<Emote>) {
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            append_text(segments, " ");
        }
        if browser::is_web_url(word) {
            segments.push(Segment::Link(word.to_owned()));
            continue;
        }
        match lookup(word) {
            Some(emote) => {
                // Une emote zero-width se pose sur l'emote précédente, même séparée
                // par un espace : on retire cet espace.
                if emote.zero_width
                    && matches!(segments.as_slice(),
                        [.., Segment::Emote { .. }, Segment::Text(t)] if t.trim().is_empty())
                {
                    segments.pop();
                }
                segments.push(Segment::Emote {
                    name: word.to_owned(),
                    url: emote.url,
                    zero_width: emote.zero_width,
                    provider: emote.provider,
                });
            }
            None => {
                for piece in emoji::split(word) {
                    match piece {
                        Piece::Text(text) => append_text(segments, text),
                        Piece::Emoji(cluster) => segments.push(Segment::Emoji {
                            text: cluster.to_owned(),
                            url: emoji::twemoji_url(cluster),
                        }),
                    }
                }
            }
        }
    }
}

/// Ajoute du texte, en le collant au morceau de texte précédent s'il y en a un.
fn append_text(segments: &mut Vec<Segment>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Segment::Text(last)) = segments.last_mut() {
        last.push_str(text);
    } else {
        segments.push(Segment::Text(text.to_owned()));
    }
}

/// Vrai si `text` contient `login` comme mot entier (ex. « @somechannel » ou « salut somechannel »).
fn mentions(text: &str, login: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let text = text.to_lowercase();
    let login = login.to_lowercase();
    text.match_indices(&login).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + login.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// Couleur du pseudo, éclaircie si elle est trop sombre pour le fond foncé.
fn readable_color(color: Option<[u8; 3]>, login: &str) -> Color32 {
    let [r, g, b] = color.unwrap_or_else(|| {
        let hash = login
            .bytes()
            .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b.into()));
        DEFAULT_COLORS[hash % DEFAULT_COLORS.len()]
    });
    let luminance = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
    let lighten = |c: u8| {
        if luminance < 110.0 {
            // Mélange avec du blanc, d'autant plus que la couleur est sombre.
            let k = (110.0 - luminance) / 160.0;
            (f32::from(c) + (255.0 - f32::from(c)) * k).round() as u8
        } else {
            c
        }
    };
    Color32::from_rgb(lighten(r), lighten(g), lighten(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn twitch(name: &str, id: &str) -> Segment {
        Segment::Emote {
            name: name.into(),
            url: twitch_emote_url(id),
            zero_width: false,
            provider: Provider::Twitch,
        }
    }

    fn third_party(name: &str, zero_width: bool) -> Emote {
        Emote {
            name: name.into(),
            url: format!("https://cdn.7tv.app/emote/{name}/1x.webp"),
            zero_width,
            provider: Provider::SevenTv,
        }
    }

    fn panel_with(emotes: &[Emote]) -> ChatPanel {
        let mut panel = ChatPanel::new(true);
        panel.set_third_party(emotes.iter().map(|e| (e.name.clone(), e.clone())).collect());
        panel
    }

    #[test]
    fn segments_avec_positions() {
        let none = |_: &str| None;
        // « é » compte pour un seul caractère : les positions Twitch sont en caractères.
        let s = build_segments("é Kappa ok", &[("25".into(), 2, 6)], &none);
        assert_eq!(
            s,
            [
                Segment::Text("é ".into()),
                twitch("Kappa", "25"),
                Segment::Text(" ok".into())
            ]
        );
        // Position hors du texte : ignorée sans planter.
        let s = build_segments("hi", &[("25".into(), 0, 50)], &none);
        assert_eq!(s, [Segment::Text("hi".into())]);
    }

    #[test]
    fn segments_par_nom_et_zero_width() {
        let emotes = HashMap::from([
            ("KEKW".to_owned(), third_party("KEKW", false)),
            ("RainTime".to_owned(), third_party("RainTime", true)),
        ]);
        let lookup = |w: &str| emotes.get(w).cloned();
        let s = build_segments("lol KEKW RainTime KEKWx", &[], &lookup);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0], Segment::Text("lol ".into()));
        assert!(matches!(&s[1], Segment::Emote { name, .. } if name == "KEKW"));
        // L'espace entre KEKW et RainTime a été retiré : RainTime se superpose à KEKW.
        assert!(
            matches!(&s[2], Segment::Emote { name, zero_width: true, .. } if name == "RainTime")
        );
        assert_eq!(s[3], Segment::Text(" KEKWx".into()));
    }

    #[test]
    fn liens() {
        let none = |_: &str| None;
        let s = build_segments("clip https://www.twitch.tv/somechannel/clip/Abc ok", &[], &none);
        assert_eq!(
            s,
            [
                Segment::Text("clip ".into()),
                Segment::Link("https://www.twitch.tv/somechannel/clip/Abc".into()),
                Segment::Text(" ok".into())
            ]
        );
        // Pas une adresse web : texte normal.
        let s = build_segments("javascript:alert(1)", &[], &none);
        assert_eq!(s, [Segment::Text("javascript:alert(1)".into())]);
    }

    #[test]
    fn autocompletion() {
        let mut panel = panel_with(&[third_party("KEKW", false), third_party("Kappa", false)]);
        panel.remember_chatter("Kevin");
        panel.input = "salut ke".into();
        assert!(panel.complete(false));
        assert_eq!(panel.input, "salut KEKW ");
        assert!(panel.complete(false));
        assert_eq!(panel.input, "salut Kevin ");
        assert!(panel.complete(true));
        assert_eq!(panel.input, "salut KEKW ");

        panel.input = "@ke".into();
        assert!(panel.complete(false));
        assert_eq!(panel.input, "@Kevin ");

        panel.input = "zzz".into();
        assert!(!panel.complete(false));
    }

    #[test]
    fn detection_des_mentions() {
        assert!(mentions("salut @Vincent !", "vincent"));
        assert!(!mentions("vincentrosio", "vincent"));
    }

    #[test]
    fn couleurs_lisibles() {
        assert_ne!(
            readable_color(Some([0, 0, 255]), "x"),
            Color32::from_rgb(0, 0, 255)
        );
        assert_eq!(
            readable_color(Some([255, 255, 0]), "x"),
            Color32::from_rgb(255, 255, 0)
        );
    }
}
