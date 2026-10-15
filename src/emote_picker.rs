//! Emote picker: browse all the available emotes and choose one's favourites.

use std::collections::HashSet;

use eframe::egui::{self, Color32, RichText, Sense, Stroke, Vec2};

use crate::emotes::{Emote, Provider};
use crate::images::ImageCache;

/// Size of a cell of the grid, and height of the image inside it.
const CELL: f32 = 36.0;
const ICON: f32 = 28.0;
const FAVORITE_BORDER: Color32 = Color32::from_rgb(240, 190, 60);

/// Emotes sorted by service then by name (rebuilt when the emotes change).
#[derive(Default)]
pub struct Catalog {
    sections: Vec<(Provider, Vec<Emote>)>,
}

impl Catalog {
    pub fn build<'a>(emotes: impl Iterator<Item = &'a Emote>) -> Catalog {
        let order = [
            Provider::Twitch,
            Provider::SevenTv,
            Provider::Bttv,
            Provider::Ffz,
        ];
        let all: Vec<&Emote> = emotes.collect();
        let sections = order
            .into_iter()
            .map(|provider| {
                let mut list: Vec<Emote> = all
                    .iter()
                    .filter(|e| e.provider == provider)
                    .map(|e| (*e).clone())
                    .collect();
                list.sort_by_cached_key(|e| e.name.to_lowercase());
                (provider, list)
            })
            .filter(|(_, list)| !list.is_empty())
            .collect();
        Catalog { sections }
    }
}

pub enum Action {
    ToggleFavorite(String),
    Insert(String),
}

/// A row of the grid: a section title or a row of emotes.
enum Row<'a> {
    Header(Provider, usize),
    Cells(Vec<&'a Emote>),
}

#[derive(Default)]
pub struct Picker {
    pub open: bool,
    search: String,
}

impl Picker {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        catalog: &Catalog,
        favorites: &[String],
        images: &mut ImageCache,
        time: f64,
    ) -> Option<Action> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Emotes").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Rechercher…")
                    .desired_width(140.0),
            );
            if ui
                .button(crate::style::icon_label(
                    egui_phosphor::regular::X,
                    "Fermer",
                ))
                .clicked()
            {
                self.open = false;
            }
        });
        ui.label(
            RichText::new("Clic : ajouter / retirer des favorites · clic droit : insérer")
                .small()
                .weak(),
        );

        let needle = self.search.to_lowercase();
        let favorites: HashSet<&str> = favorites.iter().map(String::as_str).collect();
        let width = ui.available_width();
        let columns = ((width / CELL).floor() as usize).max(1);
        let mut rows: Vec<Row<'_>> = Vec::new();
        for (provider, list) in &catalog.sections {
            let matching: Vec<&Emote> = list
                .iter()
                .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle))
                .collect();
            if matching.is_empty() {
                continue;
            }
            rows.push(Row::Header(*provider, matching.len()));
            rows.extend(
                matching
                    .chunks(columns)
                    .map(|chunk| Row::Cells(chunk.to_vec())),
            );
        }
        if rows.is_empty() {
            ui.label(RichText::new("Aucune emote.").weak());
            return None;
        }

        // All rows have the same height: only the visible rows are
        // drawn, and only their images are downloaded.
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, CELL, rows.len(), |ui, range| {
                for row in &rows[range] {
                    match row {
                        Row::Header(provider, count) => {
                            ui.allocate_ui_with_layout(
                                egui::vec2(width, CELL),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{} ({count})", provider.label()))
                                            .strong(),
                                    )
                                },
                            );
                        }
                        Row::Cells(emotes) => {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for emote in emotes {
                                    let favorite = favorites.contains(emote.name.as_str());
                                    if let Some(a) = cell_ui(ui, emote, favorite, images, time) {
                                        action = Some(a);
                                    }
                                }
                            });
                        }
                    }
                }
            });
        action
    }
}

fn cell_ui(
    ui: &mut egui::Ui,
    emote: &Emote,
    favorite: bool,
    images: &mut ImageCache,
    time: f64,
) -> Option<Action> {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(CELL), Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect.shrink(1.0), 4.0, Color32::from_gray(70));
    }
    if favorite {
        painter.rect_stroke(
            rect.shrink(1.5),
            4.0,
            Stroke::new(2.0, FAVORITE_BORDER),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(picture) = images.get(&emote.url) {
        let frame = picture.frame_at(time, false);
        let mut size = frame.size * (ICON / frame.size.y.max(1.0));
        if size.x > CELL - 4.0 {
            size *= (CELL - 4.0) / size.x;
        }
        let image_rect = egui::Rect::from_center_size(rect.center(), size);
        ui.painter()
            .image(frame.texture, image_rect, frame.uv, Color32::WHITE);
    }
    let response = response.on_hover_text(format!("{} · {}", emote.name, emote.provider.label()));
    if response.clicked() {
        Some(Action::ToggleFavorite(emote.name.clone()))
    } else if response.secondary_clicked() {
        Some(Action::Insert(emote.name.clone()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_trie_par_service() {
        let emote = |name: &str, provider| Emote {
            name: name.into(),
            url: String::new(),
            zero_width: false,
            provider,
        };
        let emotes = [
            emote("b", Provider::Bttv),
            emote("Zz", Provider::SevenTv),
            emote("aa", Provider::SevenTv),
        ];
        let catalog = Catalog::build(emotes.iter());
        assert_eq!(catalog.sections.len(), 2);
        assert_eq!(catalog.sections[0].0, Provider::SevenTv);
        let names: Vec<_> = catalog.sections[0]
            .1
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, ["aa", "Zz"]);
    }
}
