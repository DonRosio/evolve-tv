//! Panneau du chat : messages avec badges et emotes, défilement, zone de saisie.

use std::collections::{HashMap, VecDeque};

use eframe::egui::{self, Color32, RichText, Vec2, load::SizedTexture};

use crate::chat::{Badge, ChatMessage, ClearTarget};
use crate::helix::BadgeImage;
use crate::images::ImageCache;

/// Messages gardés en mémoire (les plus anciens sont oubliés).
const MAX_LINES: usize = 300;
const EMOTE_HEIGHT: f32 = 28.0;
const BADGE_SIZE: f32 = 18.0;

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
    Emote { name: String, url: String },
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
}

#[derive(Default)]
pub struct ChatPanel {
    lines: VecDeque<Line>,
    input: String,
    my_login: Option<String>,
    /// Emotes Twitch utilisables par moi : nom → identifiant.
    user_emotes: HashMap<String, String>,
    badges: HashMap<Badge, BadgeImage>,
    scroll_to_bottom: bool,
}

impl ChatPanel {
    pub fn set_me(&mut self, login: Option<String>) {
        if login.is_none() {
            self.user_emotes.clear();
        }
        self.my_login = login;
    }

    pub fn set_user_emotes(&mut self, emotes: HashMap<String, String>) {
        self.user_emotes = emotes
            .into_iter()
            .filter(|(_, id)| is_valid_emote_id(id))
            .collect();
    }

    pub fn set_badges(&mut self, badges: HashMap<Badge, BadgeImage>) {
        self.badges = badges;
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        self.push(Line {
            id: String::new(),
            login: String::new(),
            kind: LineKind::System(text.into()),
            deleted: false,
        });
    }

    pub fn push_message(&mut self, message: ChatMessage) {
        // Twitch ne renvoie pas nos propres messages, donc ni la position de leurs
        // emotes : on les reconnaît par leur nom.
        let by_name = message.own.then_some(&self.user_emotes);
        let segments = build_segments(&message.text, &message.emotes, by_name);
        let mention = !message.own
            && self
                .my_login
                .as_deref()
                .is_some_and(|me| mentions(&message.text, me));
        self.push(Line {
            id: message.id,
            kind: LineKind::Chat {
                name: message.display_name,
                color: readable_color(message.color, &message.login),
                badges: message.badges,
                segments,
                action: message.action,
                mention,
            },
            login: message.login,
            deleted: false,
        });
    }

    pub fn clear(&mut self, target: ClearTarget) {
        match target {
            ClearTarget::Message(id) => {
                for line in self.lines.iter_mut().filter(|l| l.id == id) {
                    line.deleted = true;
                }
            }
            ClearTarget::User(login) => {
                for line in self.lines.iter_mut().filter(|l| l.login == login) {
                    line.deleted = true;
                }
            }
            ClearTarget::All => self.push_system("Le chat a été effacé par un modérateur."),
        }
    }

    fn push(&mut self, line: Line) {
        self.lines.push_back(line);
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    /// Dessine le panneau. Renvoie le message à envoyer quand on appuie sur Entrée.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        images: &mut ImageCache,
        can_send: bool,
    ) -> Option<String> {
        let mut to_send = None;
        egui::Panel::bottom("chat_saisie").show(ui, |ui| {
            ui.add_space(4.0);
            let hint = if can_send {
                "Envoyer un message"
            } else {
                "Connecte-toi pour écrire"
            };
            let edit = egui::TextEdit::singleline(&mut self.input)
                .hint_text(hint)
                .char_limit(500)
                .desired_width(f32::INFINITY);
            let response = ui.add_enabled(can_send, edit);
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if !self.input.trim().is_empty() {
                    to_send = Some(std::mem::take(&mut self.input));
                    self.scroll_to_bottom = true;
                }
                response.request_focus();
            }
            ui.add_space(4.0);
        });

        let mut area = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            // Reste collé en bas… sauf si on remonte dans l'historique.
            .stick_to_bottom(true);
        if std::mem::take(&mut self.scroll_to_bottom) {
            area = area.vertical_scroll_offset(f32::MAX);
        }
        let output = area.show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            for line in &self.lines {
                self.line_ui(ui, line, images);
            }
        });

        let at_bottom =
            output.state.offset.y + output.inner_rect.height() >= output.content_size.y - 4.0;
        if !at_bottom {
            let rect = egui::Rect::from_center_size(
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

    fn line_ui(&self, ui: &mut egui::Ui, line: &Line, images: &mut ImageCache) {
        let LineKind::Chat {
            name,
            color,
            badges,
            segments,
            action,
            mention,
        } = &line.kind
        else {
            if let LineKind::System(text) = &line.kind {
                ui.label(RichText::new(text).italics().color(Color32::GRAY));
            }
            return;
        };

        let mut frame = egui::Frame::new().inner_margin(2.0);
        if *mention {
            frame = frame.fill(Color32::from_rgba_unmultiplied(220, 50, 50, 45));
        }
        frame.show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for badge in badges {
                    let Some(info) = self.badges.get(badge) else {
                        continue;
                    };
                    if let Some(texture) = images.get(&info.url) {
                        let image = egui::Image::from_texture(SizedTexture::from_handle(texture))
                            .fit_to_exact_size(Vec2::splat(BADGE_SIZE));
                        ui.add(image).on_hover_text(&info.title);
                        ui.add_space(3.0);
                    }
                }
                ui.label(RichText::new(name).color(*color).strong());
                ui.label(if *action { " " } else { ": " });
                if line.deleted {
                    ui.label(RichText::new("<message supprimé>").italics().weak());
                    return;
                }
                for segment in segments {
                    match segment {
                        Segment::Text(text) => {
                            let text = RichText::new(text);
                            ui.label(if *action {
                                text.color(*color).italics()
                            } else {
                                text
                            });
                        }
                        Segment::Emote { name, url } => match images.get(url) {
                            Some(texture) => {
                                let size = texture.size_vec2();
                                let size = size * (EMOTE_HEIGHT / size.y.max(1.0));
                                let image =
                                    egui::Image::from_texture(SizedTexture::from_handle(texture))
                                        .fit_to_exact_size(size);
                                ui.add(image).on_hover_text(name);
                            }
                            // Pas encore chargée : on affiche son nom en attendant.
                            None => {
                                ui.label(RichText::new(name).weak());
                            }
                        },
                    }
                }
            });
        });
    }
}

/// Les identifiants d'emotes Twitch ne contiennent que lettres, chiffres et `_`.
fn is_valid_emote_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn twitch_emote_url(id: &str) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/static/dark/1.0")
}

/// Découpe le texte en morceaux de texte et d'emotes.
/// `positions` : emotes Twitch avec leur position (en caractères), fournies par Twitch.
/// `by_name` : emotes à reconnaître par leur nom (mot entier).
fn build_segments(
    text: &str,
    positions: &[(String, usize, usize)],
    by_name: Option<&HashMap<String, String>>,
) -> Vec<Segment> {
    let chars: Vec<char> = text.chars().collect();
    let mut segments = Vec::new();
    let mut pos = 0;
    for (id, start, end) in positions {
        // Positions incohérentes (chevauchement, hors du texte) : ignorées.
        if *start < pos || *end >= chars.len() || !is_valid_emote_id(id) {
            continue;
        }
        push_text(
            &mut segments,
            &chars[pos..*start].iter().collect::<String>(),
            by_name,
        );
        segments.push(Segment::Emote {
            name: chars[*start..=*end].iter().collect(),
            url: twitch_emote_url(id),
        });
        pos = end + 1;
    }
    push_text(
        &mut segments,
        &chars[pos..].iter().collect::<String>(),
        by_name,
    );
    segments
}

fn push_text(segments: &mut Vec<Segment>, text: &str, by_name: Option<&HashMap<String, String>>) {
    let Some(emotes) = by_name else {
        append_text(segments, text);
        return;
    };
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            append_text(segments, " ");
        }
        match emotes.get(word) {
            Some(id) => segments.push(Segment::Emote {
                name: word.to_owned(),
                url: twitch_emote_url(id),
            }),
            None => append_text(segments, word),
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

    fn emote(name: &str, id: &str) -> Segment {
        Segment::Emote {
            name: name.into(),
            url: twitch_emote_url(id),
        }
    }

    #[test]
    fn segments_avec_positions() {
        // « é » compte pour un seul caractère : les positions Twitch sont en caractères.
        let s = build_segments("é Kappa ok", &[("25".into(), 2, 6)], None);
        assert_eq!(
            s,
            [
                Segment::Text("é ".into()),
                emote("Kappa", "25"),
                Segment::Text(" ok".into())
            ]
        );
        // Position hors du texte : ignorée sans planter.
        let s = build_segments("hi", &[("25".into(), 0, 50)], None);
        assert_eq!(s, [Segment::Text("hi".into())]);
    }

    #[test]
    fn segments_par_nom() {
        let emotes = HashMap::from([("KEKW".to_owned(), "123".to_owned())]);
        let s = build_segments("lol KEKW KEKWx", &[], Some(&emotes));
        assert_eq!(
            s,
            [
                Segment::Text("lol ".into()),
                emote("KEKW", "123"),
                Segment::Text(" KEKWx".into())
            ]
        );
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
