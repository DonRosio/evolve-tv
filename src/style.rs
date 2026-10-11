//! Apparence de l'application : thème sombre façon Twitch, polices et icônes.

use std::fs;
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Stroke, Theme, vec2};

/// Violet Twitch, utilisé pour les éléments actifs et la sélection.
pub const ACCENT: Color32 = Color32::from_rgb(145, 71, 255);
/// Fonds : panneaux, zones de saisie, survols.
const BACKGROUND: Color32 = Color32::from_rgb(24, 24, 27);
const INPUT_BACKGROUND: Color32 = Color32::from_rgb(14, 14, 16);
const WIDGET: Color32 = Color32::from_rgb(38, 38, 44);
const WIDGET_HOVER: Color32 = Color32::from_rgb(54, 54, 62);
const BORDER: Color32 = Color32::from_rgb(47, 47, 53);
const TEXT: Color32 = Color32::from_rgb(239, 239, 241);

/// Police système utilisée en secours pour les caractères absents des autres
/// polices : braille (art ASCII du chat), flèches, symboles…
const FALLBACK_FONT: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";

/// Installe le thème et les polices (à appeler une fois, au démarrage).
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, |style| {
        style.spacing.item_spacing = vec2(6.0, 4.0);
        style.spacing.button_padding = vec2(8.0, 4.0);

        let visuals = &mut style.visuals;
        visuals.panel_fill = BACKGROUND;
        visuals.window_fill = Color32::from_rgb(31, 31, 35);
        visuals.extreme_bg_color = INPUT_BACKGROUND;
        visuals.faint_bg_color = WIDGET;
        visuals.hyperlink_color = Color32::from_rgb(169, 112, 255);
        visuals.selection.bg_fill = ACCENT;
        visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        visuals.window_corner_radius = CornerRadius::same(8);
        visuals.menu_corner_radius = CornerRadius::same(6);

        let widgets = &mut visuals.widgets;
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_gray(200));
        for (state, fill) in [
            (&mut widgets.inactive, WIDGET),
            (&mut widgets.hovered, WIDGET_HOVER),
            (&mut widgets.active, ACCENT),
            (&mut widgets.open, WIDGET_HOVER),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.corner_radius = CornerRadius::same(6);
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke = Stroke::new(1.0, TEXT);
        }
        widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
    });
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // Icônes Phosphor, utilisables directement dans le texte des boutons.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    // Taille plafonnée : on ne charge pas en mémoire un fichier anormal.
    let fallback = match fs::metadata(FALLBACK_FONT) {
        Ok(meta) if meta.len() <= 4 * 1024 * 1024 => fs::read(FALLBACK_FONT).ok(),
        _ => None,
    };
    match fallback {
        Some(bytes) => {
            fonts.font_data.insert(
                "dejavu".to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            // En dernier : elle ne sert que si les autres polices n'ont pas le caractère.
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("dejavu".to_owned());
            }
        }
        None => eprintln!(
            "Police de secours absente ({FALLBACK_FONT}) : l'art ASCII peut mal s'afficher."
        ),
    }
    ctx.set_fonts(fonts);
}
