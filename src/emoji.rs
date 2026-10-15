//! Detecting emojis in the text, to show them in colour (Twemoji images).
//!
//! An "emoji" can be a sequence of several Unicode characters: 👍🏻 (thumb +
//! skin colour), 👨‍👩‍👧 (three people linked by U+200D "joiners"),
//! 🇫🇷 (two regional letters), 1️⃣ (digit + U+FE0F + U+20E3)…

/// Twemoji images (pinned version) served by the jsDelivr CDN.
pub const TWEMOJI_BASE: &str = "https://cdn.jsdelivr.net/gh/jdecked/twemoji@15.1.0/assets/72x72/";

const ZWJ: char = '\u{200D}';
const VS16: char = '\u{FE0F}'; // « afficher en emoji »
const VS15: char = '\u{FE0E}'; // « afficher en texte »
const KEYCAP: char = '\u{20E3}';

#[derive(Debug, PartialEq)]
pub enum Piece<'a> {
    Text(&'a str),
    Emoji(&'a str),
}

/// Character that can start an emoji (Unicode ranges of pictograms).
fn is_pictographic(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF
        | 0x2600..=0x27BF
        | 0x2300..=0x23FF
        | 0x2B00..=0x2BFF
        | 0x2190..=0x21FF
        | 0x25A0..=0x25FF
        | 0x2934 | 0x2935 | 0x3030 | 0x303D | 0x3297 | 0x3299
        | 0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x24C2)
}

fn is_regional_indicator(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

/// Characters that modify the previous emoji without starting a new one.
fn is_modifier(c: char) -> bool {
    matches!(c as u32,
        0xFE0E | 0xFE0F         // variantes texte / emoji
        | 0x1F3FB..=0x1F3FF    // couleurs de peau
        | 0x20E3               // touche de clavier
        | 0xE0020..=0xE007F) // tags (regional flags: Scotland…)
}

/// Splits `text` into pieces of text and emojis.
pub fn split(text: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut text_start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        let next = chars.peek().map(|&(_, n)| n);
        let begins = is_pictographic(c)
            || is_regional_indicator(c)
            // Touche de clavier : 1️⃣ = « 1 » + U+FE0F + U+20E3.
            || (matches!(c, '0'..='9' | '#' | '*') && matches!(next, Some(VS16 | KEYCAP)));
        if !begins {
            continue;
        }
        let mut end = start + c.len_utf8();
        if is_regional_indicator(c) {
            // A flag = two regional letters.
            if let Some(&(i, n)) = chars.peek()
                && is_regional_indicator(n)
            {
                end = i + n.len_utf8();
                chars.next();
            }
        }
        // Modifiers and sequences linked by joiners (U+200D).
        while let Some(&(i, n)) = chars.peek() {
            if is_modifier(n) {
                end = i + n.len_utf8();
                chars.next();
            } else if n == ZWJ {
                let mut ahead = chars.clone();
                ahead.next();
                match ahead.peek() {
                    Some(&(j, m)) if is_pictographic(m) => {
                        end = j + m.len_utf8();
                        chars.next();
                        chars.next();
                    }
                    _ => break,
                }
            } else {
                break;
            }
        }
        let cluster = &text[start..end];
        // U+FE0E demande explicitement l'affichage en texte.
        if cluster.contains(VS15) || (cluster.chars().count() == 1 && !is_emoji_alone(c)) {
            continue;
        }
        if text_start < start {
            pieces.push(Piece::Text(&text[text_start..start]));
        }
        pieces.push(Piece::Emoji(cluster));
        text_start = end;
    }
    if text_start < text.len() {
        pieces.push(Piece::Text(&text[text_start..]));
    }
    pieces
}

/// Is a lone pictogram (without U+FE0F) shown as an emoji by default?
/// Arrows and geometric shapes (→, ■…) stay text; the real
/// emojis (🤡, ⭐, ♀…) are converted.
fn is_emoji_alone(c: char) -> bool {
    !matches!(c as u32, 0x2190..=0x21FF | 0x25A0..=0x25FF | 0x2300..=0x23FF)
        || matches!(c as u32, 0x231A | 0x231B | 0x23E9..=0x23F3 | 0x23F8..=0x23FA | 0x25FD | 0x25FE)
}

/// Address of the Twemoji image: hexadecimal codes joined by `-`. Twemoji
/// convention: U+FE0F is removed, except in sequences linked by U+200D.
pub fn twemoji_url(cluster: &str) -> String {
    let keep_vs16 = cluster.contains(ZWJ);
    let code: Vec<String> = cluster
        .chars()
        .filter(|&c| keep_vs16 || c != VS16)
        .map(|c| format!("{:x}", u32::from(c)))
        .collect();
    format!("{TWEMOJI_BASE}{}.png", code.join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emojis(text: &str) -> Vec<String> {
        split(text)
            .into_iter()
            .filter_map(|p| match p {
                Piece::Emoji(e) => Some(twemoji_url(e).trim_start_matches(TWEMOJI_BASE).to_owned()),
                Piece::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn decoupage() {
        assert_eq!(
            split("salut🤡 ok"),
            [Piece::Text("salut"), Piece::Emoji("🤡"), Piece::Text(" ok")]
        );
        assert_eq!(split("rien ici"), [Piece::Text("rien ici")]);
    }

    #[test]
    fn noms_des_images() {
        assert_eq!(emojis("🤡"), ["1f921.png"]);
        assert_eq!(emojis("♀️ Bouffon ♀"), ["2640.png", "2640.png"]);
        assert_eq!(emojis("👍🏻"), ["1f44d-1f3fb.png"]);
        assert_eq!(emojis("👨‍👩‍👧"), ["1f468-200d-1f469-200d-1f467.png"]);
        assert_eq!(emojis("🤷‍♀️"), ["1f937-200d-2640-fe0f.png"]);
        assert_eq!(emojis("🇫🇷"), ["1f1eb-1f1f7.png"]);
        assert_eq!(emojis("1️⃣ mais pas 1"), ["31-20e3.png"]);
    }

    #[test]
    fn texte_conserve() {
        // Simple arrow and explicit request for text display: no image.
        assert!(emojis("a → b").is_empty());
        assert!(emojis("♀\u{FE0E}").is_empty());
    }
}
