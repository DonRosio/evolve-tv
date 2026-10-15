//! Chat panel: messages with badges and emotes (animated, zero-width),
//! scrolling, actions on hover (copy, reply), input area with
//! autocompletion (Tab).

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{
    self, Color32, FontId, Key, Modifiers, Pos2, Rect, RichText, Sense, Shape, Vec2, pos2, vec2,
};

use egui_phosphor::regular as icon;

use crate::browser;
use crate::chat::{Badge, ChatMessage, ClearTarget, ReplyInfo, ReplyTarget, is_valid_message_id};
use crate::config::{self, SavedMessage};
use crate::emoji::{self, Piece};
use crate::emote_picker::{self, Catalog, Picker};
use crate::emotes::{Emote, EmoteMap, Provider};
use crate::helix::BadgeImage;
use crate::images::{ImageCache, Picture};
use crate::style;

/// Messages kept in memory (the oldest ones are forgotten).
const MAX_LINES: usize = 300;
const MAX_CHATTERS: usize = 300;
const EMOTE_HEIGHT: f32 = 28.0;
const BADGE_SIZE: f32 = 18.0;
/// Emojis are shown at the size of the text.
const EMOJI_SIZE: f32 = 18.0;
const SUGGESTION_HEIGHT: f32 = 20.0;
/// Margin reserved on the right of each message for the Copy / Reply buttons.
const ACTIONS_WIDTH: f32 = 48.0;
/// Message text: almost white, easy to read on the dark background.
const TEXT_COLOR: Color32 = Color32::from_gray(230);
/// Background of a hovered message, and of a hovered emote (a bit lighter).
const HOVER_BG: Color32 = Color32::from_gray(45);
const EMOTE_HOVER_BG: Color32 = Color32::from_gray(80);
/// Colour of links (purple, as on Twitch).
const LINK_COLOR: Color32 = Color32::from_rgb(169, 112, 255);
/// A person's first message: pink, as on Twitch.
const FIRST_COLOR: Color32 = Color32::from_rgb(255, 117, 196);
/// Beyond this length, a word may be cut at the end of the line.
const LONG_WORD: usize = 30;
/// Animations are redrawn at most 30 times a second.
const MIN_FRAME_DELAY: f64 = 1.0 / 30.0;

/// Colours Twitch assigns to accounts that have not chosen one.
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
    /// Emoji shown in colour (Twemoji image).
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
        first: bool,
        /// ASCII art (braille characters): one drawing "line" per row.
        art: bool,
        reply: Option<ReplyInfo>,
    },
    System(String),
}

struct Line {
    /// Unique number of the line (stable identifier for the hover buttons).
    seq: u64,
    id: String,
    login: String,
    display_name: String,
    /// Plain text, for "Copy" and "Reply".
    text: String,
    kind: LineKind,
    deleted: bool,
    /// Height measured at the last display, and the matching width: makes it possible to
    /// draw only the visible lines (the others are replaced by an empty space).
    /// `Cell` allows updating it while the lines are being walked through read-only.
    height: Cell<f32>,
    width: Cell<f32>,
}

/// Autocompletion in progress (Tab / Shift+Tab cycle through the suggestions).
struct Completion {
    /// Position (in bytes) of the start of the completed word in the input.
    start: usize,
    candidates: Vec<String>,
    index: usize,
    /// Input text right after our completion: if it changes, we start over.
    applied: String,
}

/// Message to send.
pub struct Outgoing {
    pub text: String,
    pub reply_to: Option<ReplyTarget>,
}

pub struct ChatPanel {
    lines: VecDeque<Line>,
    next_seq: u64,
    input: String,
    my_login: Option<String>,
    /// Twitch emotes I can use (to recognise those in my own messages).
    user_emotes: EmoteMap,
    /// 7TV, BTTV and FFZ emotes.
    third_party: EmoteMap,
    badges: HashMap<Badge, BadgeImage>,
    /// Recently active nicknames, the most recent at the end.
    chatters: VecDeque<String>,
    completion: Option<Completion>,
    /// Clicked link, waiting for confirmation before opening.
    pending_link: Option<String>,
    /// Message being replied to.
    replying: Option<ReplyTarget>,
    line_menu: Option<LineMenu>,
    focus_input: bool,
    /// Saved-message buttons and favourite emotes (a copy of the config).
    saved_messages: Vec<SavedMessage>,
    favorites: Vec<String>,
    /// True when the shortcuts changed and must be saved.
    shortcuts_changed: bool,
    bar_collapsed: bool,
    picker: Picker,
    catalog: Catalog,
    scroll_to_bottom: bool,
    animate: bool,
}

/// What the drawing of the lines must signal to the panel.
#[derive(Default)]
struct DrawOutput {
    /// Delay before the next frame of a visible animated emote.
    next_repaint: Option<f64>,
    clicked_link: Option<String>,
    reply: Option<ReplyTarget>,
    menu: Option<LineMenu>,
}

/// Menu opened by a right click on a message.
struct LineMenu {
    pos: Pos2,
    target: ReplyTarget,
    can_reply: bool,
}

impl ChatPanel {
    pub fn new(animate: bool) -> ChatPanel {
        ChatPanel {
            lines: VecDeque::new(),
            next_seq: 0,
            input: String::new(),
            my_login: None,
            user_emotes: EmoteMap::new(),
            third_party: EmoteMap::new(),
            badges: HashMap::new(),
            chatters: VecDeque::new(),
            completion: None,
            pending_link: None,
            replying: None,
            line_menu: None,
            focus_input: false,
            saved_messages: Vec::new(),
            favorites: Vec::new(),
            shortcuts_changed: false,
            bar_collapsed: false,
            picker: Picker::default(),
            catalog: Catalog::default(),
            scroll_to_bottom: false,
            animate,
        }
    }

    pub fn set_me(&mut self, login: Option<String>) {
        if login.is_none() {
            self.user_emotes.clear();
            self.replying = None;
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
        self.rebuild_catalog();
    }

    pub fn set_third_party(&mut self, emotes: EmoteMap) {
        self.third_party = emotes;
        self.rebuild_catalog();
    }

    fn rebuild_catalog(&mut self) {
        self.catalog = Catalog::build(self.user_emotes.values().chain(self.third_party.values()));
    }

    pub fn set_shortcuts(&mut self, saved_messages: Vec<SavedMessage>, favorites: Vec<String>) {
        self.saved_messages = saved_messages;
        self.favorites = favorites;
    }

    /// Shortcuts modified from the interface, to be saved in the config.
    pub fn take_shortcuts_change(&mut self) -> Option<(Vec<SavedMessage>, Vec<String>)> {
        std::mem::take(&mut self.shortcuts_changed)
            .then(|| (self.saved_messages.clone(), self.favorites.clone()))
    }

    pub fn set_badges(&mut self, badges: HashMap<Badge, BadgeImage>) {
        self.badges = badges;
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.push(Line {
            seq: 0,
            id: String::new(),
            login: String::new(),
            display_name: String::new(),
            text: text.clone(),
            kind: LineKind::System(text),
            deleted: false,
            height: Cell::new(0.0),
            width: Cell::new(0.0),
        });
    }

    pub fn push_message(&mut self, message: ChatMessage) {
        let own = message.own;
        let art = is_ascii_art(&message.text);
        // 7TV/BTTV/FFZ emotes are recognised by their name in all messages.
        // Twitch does not send our own messages back, hence neither the position of their
        // Twitch emotes: they are recognised by their name too.
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
            name: message.display_name.clone(),
            color: readable_color(message.color, &message.login),
            badges: message.badges,
            segments,
            action: message.action,
            mention,
            first: message.first,
            art,
            reply: message.reply,
        };
        self.push(Line {
            seq: 0,
            id: message.id,
            login: message.login,
            display_name: message.display_name,
            text: message.text,
            kind,
            deleted: false,
            height: Cell::new(0.0),
            width: Cell::new(0.0),
        });
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
            line.height.set(0.0); // to be measured again
        }
    }

    fn push(&mut self, mut line: Line) {
        self.next_seq += 1;
        line.seq = self.next_seq;
        self.lines.push_back(line);
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    // ---------- Autocompletion ----------

    fn emote_named(&self, name: &str) -> Option<&Emote> {
        self.user_emotes
            .get(name)
            .or_else(|| self.third_party.get(name))
    }

    /// Suggestions for `word`: emotes then nicknames (or only nicknames with `@`).
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

    /// Completes the last word of the input. Returns `true` if the input changed.
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

    /// Prepares a reply: "@nickname" at the start of the input, cursor at the end.
    fn start_reply(&mut self, target: ReplyTarget) {
        let mention = format!("@{} ", target.name);
        if !self.input.starts_with(&mention) {
            self.input = format!("{mention}{}", self.input);
        }
        self.replying = Some(target);
        self.focus_input = true;
    }

    // ---------- Affichage ----------

    /// Draws the panel. Returns the message to send when Enter is pressed.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        images: &mut ImageCache,
        can_send: bool,
    ) -> Option<Outgoing> {
        let time = ui.input(|i| i.time);
        let mut to_send = None;

        egui::Panel::bottom("chat_saisie").show(ui, |ui| {
            ui.add_space(4.0);
            self.reply_bar_ui(ui);
            self.link_confirmation_ui(ui);
            self.suggestions_ui(ui, images, time);
            let id = egui::Id::new("chat_saisie_texte");
            if can_send && std::mem::take(&mut self.focus_input) {
                ui.memory_mut(|m| m.request_focus(id));
                move_cursor_to_end(ui.ctx(), id, &self.input);
            }
            if can_send && ui.memory(|m| m.has_focus(id)) {
                // Tab is intercepted before the input field, which therefore does not see it.
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
                // Tab does not leave the field (it is used for autocompletion).
                .lock_focus(true)
                // A bit taller than the default, more comfortable.
                .margin(egui::Margin::symmetric(8, 7))
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
                    to_send = Some(Outgoing {
                        text: std::mem::take(&mut self.input),
                        reply_to: self.replying.take(),
                    });
                    self.completion = None;
                    self.scroll_to_bottom = true;
                }
                response.request_focus();
            }
            if let Some(outgoing) = self.shortcuts_ui(ui, images, time, can_send) {
                to_send = Some(outgoing);
            }
            ui.add_space(4.0);
        });

        if self.picker.open {
            let action = self
                .picker
                .ui(ui, &self.catalog, &self.favorites, images, time);
            if let Some(action) = action {
                self.apply_picker(action);
            }
        } else {
            self.messages_ui(ui, images, time, can_send);
        }
        to_send
    }

    /// List of messages (only the visible lines are drawn).
    fn messages_ui(
        &mut self,
        ui: &mut egui::Ui,
        images: &mut ImageCache,
        time: f64,
        can_send: bool,
    ) {
        let mut out = DrawOutput::default();
        let scroll_to_bottom = std::mem::take(&mut self.scroll_to_bottom);
        let area = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            // Stays stuck to the bottom… unless one scrolls up in the history.
            .stick_to_bottom(true);
        let output = area.show_viewport(ui, |ui, viewport| {
            ui.visuals_mut().override_text_color = Some(TEXT_COLOR);
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
                    // Off-screen line: a simple space of the right height.
                    ui.allocate_space(egui::vec2(width, height));
                } else {
                    let rect = self.line_ui(ui, line, images, time, can_send, &mut out);
                    line.height.set(rect.height());
                    line.width.set(width);
                }
                y += line.height.get() + spacing;
            }
            if scroll_to_bottom {
                // Scrolls to the real end of the content (the scrolling then
                // "sticks" back to the bottom of the chat).
                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
            }
        });
        if let Some(url) = out.clicked_link.take() {
            self.pending_link = Some(url);
        }
        if let Some(menu) = out.menu.take() {
            self.line_menu = Some(menu);
        }
        if let Some(target) = out.reply.take() {
            self.start_reply(target);
        }
        if let Some(target) = self.line_menu_ui(ui.ctx()) {
            self.start_reply(target);
        }

        // Redraws just in time for the next frame of the visible animated emotes.
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
    }

    /// Bar under the input: saved messages and favourite emotes.
    fn shortcuts_ui(
        &mut self,
        ui: &mut egui::Ui,
        images: &mut ImageCache,
        time: f64,
        can_send: bool,
    ) -> Option<Outgoing> {
        let mut outgoing = None;
        let mut insert: Option<String> = None;
        let mut remove: Option<usize> = None;
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let toggle = if self.bar_collapsed {
                // Collapsed: small arrow, unchanged.
                ui.small_button(style::icon(icon::CARET_RIGHT))
            } else {
                // Expanded: same height as the buttons next to it.
                ui.add(egui::Button::new(style::icon(icon::CARET_DOWN)))
            };
            if toggle
                .on_hover_text("Afficher / masquer les raccourcis")
                .clicked()
            {
                self.bar_collapsed = !self.bar_collapsed;
            }
            if self.bar_collapsed {
                ui.label(RichText::new("Raccourcis").weak());
                return;
            }
            for (i, saved) in self.saved_messages.iter().enumerate() {
                let label = saved
                    .label
                    .clone()
                    .unwrap_or_else(|| truncate(&saved.text, 20));
                let response = ui
                    .add_enabled(can_send, egui::Button::new(label))
                    .on_hover_text(format!("{}\nClic droit : insérer ou supprimer", saved.text));
                if response.clicked() {
                    outgoing = Some(Outgoing {
                        text: saved.text.clone(),
                        reply_to: None,
                    });
                }
                response.context_menu(|ui| {
                    if ui.button("Insérer dans la saisie").clicked() {
                        insert = Some(saved.text.clone());
                        ui.close();
                    }
                    if ui.button("Supprimer ce bouton").clicked() {
                        remove = Some(i);
                        ui.close();
                    }
                });
            }
            let draft = self.input.trim();
            let can_save = config::is_valid_saved_text(draft) && self.saved_messages.len() < 50;
            if ui
                .add_enabled(can_save, egui::Button::new(style::icon(icon::PLUS)))
                .on_hover_text("Enregistrer le texte de la saisie comme bouton")
                .clicked()
            {
                self.saved_messages.push(SavedMessage {
                    label: None,
                    text: draft.to_owned(),
                });
                self.shortcuts_changed = true;
            }
        });

        if !self.bar_collapsed {
            ui.horizontal_wrapped(|ui| {
                for name in &self.favorites {
                    let picture = self.emote_named(name).and_then(|e| images.get(&e.url));
                    let response = match picture {
                        Some(picture) => {
                            let frame = picture.frame_at(time, false);
                            let size = frame.size * (24.0 / frame.size.y.max(1.0));
                            let (rect, response) =
                                ui.allocate_exact_size(size + egui::vec2(4.0, 4.0), Sense::click());
                            if response.hovered() {
                                ui.painter().rect_filled(rect, 4.0, EMOTE_HOVER_BG);
                            }
                            let image_rect = Rect::from_center_size(rect.center(), size);
                            ui.painter()
                                .image(frame.texture, image_rect, frame.uv, Color32::WHITE);
                            response
                        }
                        // Emote not found (not loaded yet, or removed from the channel).
                        None => ui.add(egui::Button::new(name.as_str()).small()),
                    };
                    let response = response.on_hover_text(format!(
                        "{name}\nClic : envoyer · clic droit : insérer dans la saisie"
                    ));
                    if response.clicked() && can_send {
                        outgoing = Some(Outgoing {
                            text: name.clone(),
                            reply_to: None,
                        });
                    } else if response.secondary_clicked() {
                        insert = Some(name.clone());
                    }
                }
                let label = if self.picker.open {
                    "Retour au chat"
                } else {
                    "Emotes"
                };
                // Same height (28 px) as the favourite emotes next to it.
                if ui
                    .add(egui::Button::new(label).min_size(egui::vec2(0.0, 28.0)))
                    .on_hover_text("Parcourir toutes les emotes et choisir tes favorites")
                    .clicked()
                {
                    self.picker.open = !self.picker.open;
                }
            });
        }

        if let Some(i) = remove {
            self.saved_messages.remove(i);
            self.shortcuts_changed = true;
        }
        if let Some(text) = insert {
            self.insert_text(&text);
        }
        if outgoing.is_some() {
            self.scroll_to_bottom = true;
        }
        outgoing
    }

    /// Appends text to the input (separated by a space) and puts the cursor there.
    fn insert_text(&mut self, text: &str) {
        if !self.input.is_empty() && !self.input.ends_with(' ') {
            self.input.push(' ');
        }
        self.input.push_str(text);
        self.input.push(' ');
        self.focus_input = true;
    }

    fn apply_picker(&mut self, action: emote_picker::Action) {
        match action {
            emote_picker::Action::ToggleFavorite(name) => {
                match self.favorites.iter().position(|n| *n == name) {
                    Some(i) => {
                        self.favorites.remove(i);
                    }
                    None if self.favorites.len() < 200 => self.favorites.push(name),
                    None => {}
                }
                self.shortcuts_changed = true;
            }
            emote_picker::Action::Insert(name) => self.insert_text(&name),
        }
    }

    /// Right-click menu on a message. Returns the message to reply to.
    fn line_menu_ui(&mut self, ctx: &egui::Context) -> Option<ReplyTarget> {
        let menu = self.line_menu.as_ref()?;
        let mut close = false;
        let mut reply = None;
        let area = egui::Area::new(egui::Id::new("menu_message"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.pos)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_min_width(170.0);
                    let item = |ui: &mut egui::Ui, glyph: &str, label: &str| {
                        ui.add(egui::Button::new(style::icon_label(glyph, label)).frame(false))
                            .clicked()
                    };
                    if item(ui, icon::COPY, "Copier le message") {
                        ctx.copy_text(menu.target.body.clone());
                        close = true;
                    }
                    if item(ui, icon::USER, "Copier le pseudo") {
                        ctx.copy_text(menu.target.name.clone());
                        close = true;
                    }
                    if menu.can_reply && item(ui, icon::ARROW_BEND_UP_LEFT, "Répondre") {
                        reply = Some(menu.target.clone());
                        close = true;
                    }
                });
            });
        // A click elsewhere or Escape closes the menu.
        let clicked_outside =
            ctx.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer();
        if close || clicked_outside || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.line_menu = None;
        }
        reply
    }

    /// "Reply to @nickname" banner above the input.
    fn reply_bar_ui(&mut self, ui: &mut egui::Ui) {
        let Some(target) = &self.replying else {
            return;
        };
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "Réponse à @{} : {}",
                    target.name,
                    truncate(&target.body, 60)
                ))
                .small(),
            );
            if ui.small_button("Annuler").clicked() {
                cancel = true;
            }
        });
        if cancel {
            self.replying = None;
        }
    }

    /// "Open this link?" bar: shows the real domain before opening the browser.
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
            ui.label(RichText::new(truncate(&url, 120)).small().weak());
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

    /// Strip of autocompletion suggestions, above the input.
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

    /// Draws a line and returns the rectangle it takes up.
    fn line_ui(
        &self,
        ui: &mut egui::Ui,
        line: &Line,
        images: &mut ImageCache,
        time: f64,
        can_reply: bool,
        out: &mut DrawOutput,
    ) -> Rect {
        let LineKind::Chat {
            name,
            color,
            badges,
            segments,
            action,
            mention,
            first,
            art,
            reply,
        } = &line.kind
        else {
            let LineKind::System(text) = &line.kind else {
                unreachable!()
            };
            return ui
                .label(RichText::new(text).italics().color(Color32::GRAY))
                .rect;
        };
        let (color, action, art) = (*color, *action, *art);

        // Space reserved under the message: filled on hover (lighter background).
        let hover_background = ui.painter().add(Shape::Noop);
        let mut frame = egui::Frame::new().inner_margin(2.0);
        if *first {
            frame = frame.fill(FIRST_COLOR.gamma_multiply(0.18));
        } else if *mention {
            frame = frame.fill(Color32::from_rgba_unmultiplied(220, 50, 50, 45));
        }
        let rect = frame
            .show(ui, |ui| {
                // Background (hover, first message, mention) over the whole width of the chat.
                ui.set_min_width(ui.available_width());
                // The hover buttons have their own margin on the right: the text
                // never goes under them.
                let content_width = (ui.available_width() - ACTIONS_WIDTH).max(80.0);
                ui.scope(|ui| {
                    ui.set_max_width(content_width);
                    if *first {
                        ui.label(
                            RichText::new("Premier message")
                                .small()
                                .strong()
                                .color(FIRST_COLOR),
                        );
                    }
                    if let Some(reply) = reply {
                        ui.label(
                            RichText::new(format!(
                                "En réponse à @{} : {}",
                                reply.parent_name,
                                truncate(&reply.parent_body, 80)
                            ))
                            .small()
                            .weak(),
                        );
                    }
                    if art && !line.deleted {
                        // The nickname and the first drawing share the same top; the rest of the
                        // drawing lines up below, under the nickname.
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            self.header_ui(ui, badges, name, color, action, images, time);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                self.segments_ui(
                                    ui, segments, color, action, true, images, time, out,
                                );
                            });
                        });
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            self.header_ui(ui, badges, name, color, action, images, time);
                            if line.deleted {
                                ui.label(RichText::new("<message supprimé>").italics().weak());
                                return;
                            }
                            self.segments_ui(ui, segments, color, action, false, images, time, out);
                        });
                    }
                });
            })
            .response
            .rect;

        if ui.rect_contains_pointer(rect) {
            ui.painter()
                .set(hover_background, Shape::rect_filled(rect, 3.0, HOVER_BG));
            self.hover_actions_ui(ui, line, rect, can_reply, out);
            // Right click anywhere on the message: Copy / Reply menu.
            if ui.input(|i| i.pointer.secondary_clicked())
                && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
            {
                out.menu = Some(LineMenu {
                    pos,
                    target: ReplyTarget {
                        id: line.id.clone(),
                        name: line.display_name.clone(),
                        body: line.text.clone(),
                    },
                    can_reply: can_reply && !line.deleted && is_valid_message_id(&line.id),
                });
            }
        }
        rect
    }

    #[allow(clippy::too_many_arguments)]
    fn segments_ui(
        &self,
        ui: &mut egui::Ui,
        segments: &[Segment],
        color: Color32,
        action: bool,
        art: bool,
        images: &mut ImageCache,
        time: f64,
        out: &mut DrawOutput,
    ) {
        // Last emote drawn: a zero-width emote lands on top of it.
        let mut last_emote: Option<Rect> = None;
        for segment in segments {
            match segment {
                Segment::Text(text) if art => {
                    for row in text.split(' ').filter(|r| !r.is_empty()) {
                        ui.add(egui::Label::new(row).wrap_mode(egui::TextWrapMode::Extend));
                    }
                }
                Segment::Text(text) => {
                    if !text.trim().is_empty() {
                        last_emote = None;
                    }
                    // One label per word: the line break happens between
                    // words, as on Twitch (only very long words are cut).
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
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(EMOJI_SIZE), Sense::hover());
                            if ui.is_rect_visible(rect) {
                                ui.painter()
                                    .image(frame.texture, rect, frame.uv, Color32::WHITE);
                            }
                        }
                        // Loading, or unknown to Twemoji: the character as text.
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
                        let label = (name.as_str(), provider.label());
                        let rect = self.paint_emote(ui, picture, base, label, time, out);
                        if base.is_none() {
                            last_emote = Some(rect);
                        }
                    }
                    // Not loaded yet: its name in the meantime.
                    None => {
                        ui.label(RichText::new(name).weak());
                        last_emote = None;
                    }
                },
            }
        }
    }

    /// Badges, nickname and colon, aligned like the message text.
    #[allow(clippy::too_many_arguments)]
    fn header_ui(
        &self,
        ui: &mut egui::Ui,
        badges: &[Badge],
        name: &str,
        color: Color32,
        action: bool,
        images: &mut ImageCache,
        time: f64,
    ) {
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
    }

    /// "Copy" and "Reply" buttons at the top right of the hovered message.
    fn hover_actions_ui(
        &self,
        ui: &mut egui::Ui,
        line: &Line,
        rect: Rect,
        can_reply: bool,
        out: &mut DrawOutput,
    ) {
        let reply_allowed = can_reply && !line.deleted && is_valid_message_id(&line.id);
        let size = 20.0;
        let mut right = rect.right() - 4.0;
        let mut actions: Vec<(&str, Icon)> = vec![("Copier le message", Icon::Copy)];
        if reply_allowed {
            actions.push(("Répondre", Icon::Reply));
        }
        for (tooltip, icon) in actions {
            let button =
                Rect::from_min_size(pos2(right - size, rect.top() + 2.0), Vec2::splat(size));
            right -= size + 4.0;
            let response = ui
                .interact(
                    button,
                    egui::Id::new(("ligne", line.seq, tooltip)),
                    Sense::click(),
                )
                .on_hover_text(tooltip);
            let background = if response.hovered() {
                Color32::from_gray(95)
            } else {
                Color32::from_gray(65)
            };
            ui.painter().rect_filled(button, 4.0, background);
            let glyph = match icon {
                Icon::Copy => icon::COPY,
                Icon::Reply => icon::ARROW_BEND_UP_LEFT,
            };
            ui.painter().text(
                button.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                style::icon_font(14.0),
                Color32::WHITE,
            );
            if response.clicked() {
                match icon {
                    Icon::Copy => ui.ctx().copy_text(line.text.clone()),
                    Icon::Reply => {
                        out.reply = Some(ReplyTarget {
                            id: line.id.clone(),
                            name: line.display_name.clone(),
                            body: line.text.clone(),
                        });
                    }
                }
            }
        }
    }

    /// Draws an emote (the current frame if it is animated). With `overlay_on`,
    /// it is centred on that area (zero-width emote) without taking up space.
    /// On hover: lighter background and the emote's name shown above.
    fn paint_emote(
        &self,
        ui: &mut egui::Ui,
        picture: &Picture,
        overlay_on: Option<Rect>,
        (name, provider): (&str, &str),
        time: f64,
        out: &mut DrawOutput,
    ) -> Rect {
        let frame = picture.frame_at(time, self.animate);
        let size = frame.size * (EMOTE_HEIGHT / frame.size.y.max(1.0));
        let (rect, hovered) = match overlay_on {
            Some(base) => (Rect::from_center_size(base.center(), size), false),
            None => {
                let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
                (rect, response.hovered())
            }
        };
        // Only the emotes visible on screen are drawn and animated.
        if ui.is_rect_visible(rect) {
            if hovered {
                ui.painter()
                    .rect_filled(rect.expand(2.0), 4.0, EMOTE_HOVER_BG);
                emote_tooltip(ui, rect, name, provider);
            }
            ui.painter()
                .image(frame.texture, rect, frame.uv, Color32::WHITE);
            if let Some(delay) = frame.next_change {
                out.next_repaint = Some(out.next_repaint.map_or(delay, |d| d.min(delay)));
            }
        }
        rect
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Copy,
    Reply,
}

/// The emote's name (and its service) shown above it, in the chat column.
fn emote_tooltip(ui: &egui::Ui, rect: Rect, name: &str, provider: &str) {
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("survol_emote"));
    let painter = ui.ctx().layer_painter(layer);
    let title = painter.layout_no_wrap(name.to_owned(), FontId::proportional(14.0), Color32::WHITE);
    let subtitle = painter.layout_no_wrap(
        provider.to_owned(),
        FontId::proportional(11.0),
        Color32::from_gray(170),
    );
    let width = title.size().x.max(subtitle.size().x);
    let height = title.size().y + subtitle.size().y;
    // Above the emote, without going past the left of the column (the video is there).
    let left = ui.clip_rect().left() + 2.0;
    let x = (rect.center().x - width / 2.0).max(left);
    let top = rect.top() - height - 10.0;
    let background = Rect::from_min_size(pos2(x, top), vec2(width, height)).expand(5.0);
    painter.rect_filled(background, 4.0, Color32::from_gray(20));
    painter.galley(
        pos2(x + (width - title.size().x) / 2.0, top),
        title,
        Color32::WHITE,
    );
    painter.galley(
        pos2(
            x + (width - subtitle.size().x) / 2.0,
            top + height - subtitle.size().y,
        ),
        subtitle,
        Color32::from_gray(170),
    );
}

/// Message made mostly of braille characters (U+2800 to U+28FF): ASCII art.
fn is_ascii_art(text: &str) -> bool {
    let visible = text.chars().filter(|c| !c.is_whitespace()).count();
    let braille = text
        .chars()
        .filter(|c| ('\u{2800}'..='\u{28FF}').contains(c))
        .count();
    visible >= 20 && braille * 10 >= visible * 8
}

/// Cuts a text that is too long (in characters) and adds "…".
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut cut: String = text.chars().take(max).collect();
        cut.push('…');
        cut
    }
}

/// Short word: never cut (it wraps whole). Very long word (link…):
/// cut where needed to stay entirely visible.
fn wrap_mode(word: &str) -> egui::TextWrapMode {
    if word.chars().count() > LONG_WORD {
        egui::TextWrapMode::Wrap
    } else {
        egui::TextWrapMode::Extend
    }
}

/// Puts the cursor of the input area at the end of the text.
fn move_cursor_to_end(ctx: &egui::Context, id: egui::Id, text: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let end = CCursor::new(text.chars().count());
        state.cursor.set_char_range(Some(CCursorRange::one(end)));
        state.store(ctx, id);
    }
}

/// Twitch emote identifiers only contain letters, digits and `_`.
fn is_valid_emote_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `default`: GIF if the emote is animated, PNG otherwise.
fn twitch_emote_url(id: &str) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/1.0")
}

/// Splits the text into pieces of text and emotes.
/// `positions`: Twitch emotes with their position (in characters), provided by Twitch.
/// `lookup`: emotes to recognise by their name (whole word).
fn build_segments(
    text: &str,
    positions: &[(String, usize, usize)],
    lookup: &dyn Fn(&str) -> Option<Emote>,
) -> Vec<Segment> {
    let chars: Vec<char> = text.chars().collect();
    let mut segments = Vec::new();
    let mut pos = 0;
    for (id, start, end) in positions {
        // Inconsistent positions (overlapping, outside the text): ignored.
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
                // A zero-width emote lands on the previous emote, even separated
                // by a space: that space is removed.
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

/// Appends text, gluing it to the previous piece of text if there is one.
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

/// True if `text` contains `login` as a whole word (e.g. "@alice" or "hi alice").
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

/// Colour of the nickname, lightened if it is too dark for the dark background.
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
            // Mixes with white, the more so the darker the colour.
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
        // "é" counts as a single character: Twitch positions are in characters.
        let s = build_segments("é Kappa ok", &[("25".into(), 2, 6)], &none);
        assert_eq!(
            s,
            [
                Segment::Text("é ".into()),
                twitch("Kappa", "25"),
                Segment::Text(" ok".into())
            ]
        );
        // Position outside the text: ignored without crashing.
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
        // The space between KEKW and RainTime was removed: RainTime lands on KEKW.
        assert!(
            matches!(&s[2], Segment::Emote { name, zero_width: true, .. } if name == "RainTime")
        );
        assert_eq!(s[3], Segment::Text(" KEKWx".into()));
    }

    #[test]
    fn liens() {
        let none = |_: &str| None;
        let s = build_segments(
            "clip https://www.twitch.tv/somechannel/clip/Abc ok",
            &[],
            &none,
        );
        assert_eq!(
            s,
            [
                Segment::Text("clip ".into()),
                Segment::Link("https://www.twitch.tv/somechannel/clip/Abc".into()),
                Segment::Text(" ok".into())
            ]
        );
        // Not a web address: normal text.
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
    fn art_ascii() {
        let art = "⣿⣿⣿⣿⣿⣿⡿⠿⠛⠛⠛⠻⢿⣿⣿⣿ ⣿⣿⣿⣿⠋⠄⠄⠄⠄⠄⠄⠄⠄⠉⣿⣿";
        assert!(is_ascii_art(art));
        assert!(!is_ascii_art("salut ⣿ ça va tout le monde ?"));
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("abc", 3), "abc");
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
