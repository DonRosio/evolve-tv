//! Apparence de l'application : thème sombre façon Twitch, polices et icônes.

use std::fs;
use std::sync::Arc;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontFamily, FontId, RichText, Stroke, Theme, vec2,
};

/// Violet Twitch, utilisé pour les éléments actifs et la sélection.
pub const ACCENT: Color32 = Color32::from_rgb(145, 71, 255);
/// Fonds : panneaux, zones de saisie, survols.
const BACKGROUND: Color32 = Color32::from_rgb(24, 24, 27);
const INPUT_BACKGROUND: Color32 = Color32::from_rgb(14, 14, 16);
const WIDGET: Color32 = Color32::from_rgb(38, 38, 44);
const WIDGET_HOVER: Color32 = Color32::from_rgb(54, 54, 62);
const BORDER: Color32 = Color32::from_rgb(47, 47, 53);
const TEXT: Color32 = Color32::from_rgb(239, 239, 241);

/// Famille de police des icônes Phosphor. Les icônes sont toujours dessinées avec
/// elle explicitement : Inter contient quelques symboles aux mêmes codes, et
/// Phosphor contient des lettres (pour ses ligatures) — aucune ne doit masquer l'autre.
const ICON_FAMILY: &str = "phosphor";
/// Taille du texte normal d'egui.
const TEXT_SIZE: f32 = 12.5;

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICON_FAMILY.into()))
}

/// Icône seule (ex. `style::icon(icon::PLAY).size(18.0)`).
pub fn icon(glyph: &str) -> RichText {
    RichText::new(glyph).family(FontFamily::Name(ICON_FAMILY.into()))
}

/// Icône suivie d'un texte, chacun dans sa police.
pub fn icon_label(glyph: &str, label: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(glyph, 0.0, part(icon_font(TEXT_SIZE + 1.0)));
    job.append(label, 6.0, part(FontId::proportional(TEXT_SIZE)));
    job
}

/// Texte suivi d'une icône (ex. le chevron d'un menu).
pub fn label_icon(label: &str, glyph: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(label, 0.0, part(FontId::proportional(TEXT_SIZE)));
    job.append(glyph, 6.0, part(icon_font(TEXT_SIZE + 1.0)));
    job
}

fn part(font_id: FontId) -> TextFormat {
    TextFormat {
        font_id,
        // Couleur « à remplacer » : le bouton y met sa propre couleur de texte.
        color: Color32::PLACEHOLDER,
        valign: Align::Center,
        ..Default::default()
    }
}

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
    // Police principale : Inter (licence SIL OFL, voir assets/fonts/Inter-LICENSE.txt),
    // intégrée au programme. Les polices d'egui restent en secours derrière elle.
    fonts.font_data.insert(
        "inter".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/InterVariable.ttf"
        ))),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "inter".to_owned());
    // Icônes Phosphor, utilisables directement dans le texte des boutons.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // Les icônes n'utilisent que leur propre famille (voir `ICON_FAMILY`) : on les
    // retire du texte normal, où leurs lettres de ligature n'ont rien à faire.
    if let Some(proportional) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        proportional.retain(|name| name != ICON_FAMILY);
    }
    // La crate construit la famille des icônes à partir de celle du texte (Inter en
    // tête) : on la redéfinit avec Phosphor seule, pour qu'aucune autre police ne passe devant.
    fonts.families.insert(
        FontFamily::Name(ICON_FAMILY.into()),
        vec![ICON_FAMILY.to_owned()],
    );

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
