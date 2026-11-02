//! Repérage des raffles annoncées par le bot DollexBot dans le chat.
//!
//! Exemple d'annonce : `Raffle pour 777 DOLLEX ! 20 secondes restantes. Go "!join"`
//!
//! Le programme n'envoie jamais le texte d'une annonce : il n'extrait que le mot-clé
//! (`!join`), et seulement s'il a la forme la plus simple possible.

/// Identifiant de connexion du bot (en minuscules). C'est l'identifiant, et non le
/// nom affiché, qui fait foi : un spectateur ne peut pas le falsifier.
pub const BOT_LOGIN: &str = "dollexbot";

/// Une raffle annoncée.
#[derive(Debug, PartialEq)]
pub struct Raffle {
    /// Mot à envoyer pour participer, par exemple `!join`.
    pub keyword: String,
    /// Secondes restantes d'après l'annonce, si elle les indique.
    pub seconds_left: Option<u32>,
}

/// Lit une annonce de raffle. `None` si le texte n'en est pas une.
pub fn parse(text: &str) -> Option<Raffle> {
    if !text.to_lowercase().contains("raffle") {
        return None;
    }
    let keyword = text
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| matches!(c, '"' | '\'' | '“' | '”' | '.' | ',')))
        .find_map(|word| {
            let name = word.strip_prefix('!')?;
            let valid = (1..=20).contains(&name.chars().count())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            valid.then(|| format!("!{}", name.to_lowercase()))
        })?;
    Some(Raffle {
        keyword,
        seconds_left: seconds_left(text),
    })
}

/// Repère « 20 secondes » (ou « 1 seconde ») dans le texte.
fn seconds_left(text: &str) -> Option<u32> {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.windows(2).find_map(|pair| {
        let number: u32 = pair[0].parse().ok()?;
        pair[1]
            .to_lowercase()
            .starts_with("seconde")
            .then_some(number)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annonce_de_raffle() {
        let raffle = parse("Raffle pour 777 DOLLEX ! 20 secondes restantes. Go \"!join\"").unwrap();
        assert_eq!(raffle.keyword, "!join");
        assert_eq!(raffle.seconds_left, Some(20));
    }

    #[test]
    fn ignore_le_reste() {
        assert!(parse("salut tout le monde").is_none());
        assert!(
            parse("!join").is_none(),
            "pas une annonce : pas de mot « raffle »"
        );
    }

    #[test]
    fn refuse_les_mots_cles_dangereux() {
        // Seul le mot-clé est envoyé, jamais le reste du texte : « --help » est ignoré.
        assert_eq!(
            parse("Raffle ! Go \"!join --help\"").unwrap().keyword,
            "!join"
        );
        // Un mot-clé avec un symbole ou trop long n'est pas reconnu.
        assert!(parse("Raffle ! Go \"!jo;in\"").is_none());
        let long = format!("Raffle ! Go \"!{}\"", "a".repeat(21));
        assert!(parse(&long).is_none());
    }

    #[test]
    fn sans_compte_a_rebours() {
        let raffle = parse("Raffle pour 1111 BOOSTEX ! Go \"!join\"").unwrap();
        assert_eq!(raffle.seconds_left, None);
    }
}
