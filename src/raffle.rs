//! Repérage des raffles annoncées par le bot DollexBot dans le chat.
//!
//! Exemple d'annonce : `Raffle pour 777 DOLLEX ! 20 secondes restantes. Go "!join"`
//!
//! Le programme n'envoie jamais le texte d'une annonce : il n'extrait que le mot-clé
//! (`!join`), et seulement s'il a la forme la plus simple possible.

/// Identifiant de connexion du bot (en minuscules). C'est l'identifiant, et non le
/// nom affiché, qui fait foi : un spectateur ne peut pas le falsifier.
pub const BOT_LOGIN: &str = "dollexbot";
/// The only message the application ever sends for a raffle.
pub const JOIN_KEYWORD: &str = "!join";
/// Delay between the announcement and the « !join », in seconds.
pub const DELAY_SECS: u32 = 2;

/// Une raffle annoncée.
#[derive(Debug, PartialEq)]
pub struct Raffle {
    /// Keyword announced to take part, for example `!join`.
    pub keyword: String,
    /// Seconds left according to the announcement.
    pub seconds_left: u32,
}

/// Lit une annonce de raffle. `None` si le texte n'en est pas une.
///
/// Seule l'annonce avec compte à rebours est reconnue (« … secondes restantes … »).
/// Les autres messages du bot (gagnants, fin de raffle, etc.) sont ignorés.
pub fn parse(text: &str) -> Option<Raffle> {
    let lower = text.to_lowercase();
    if !lower.contains("raffle") || !lower.contains("restante") {
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
        seconds_left: seconds_left(text)?,
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
        assert_eq!(raffle.seconds_left, 20);
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
            parse("Raffle ! 20 secondes restantes. Go \"!join --help\"")
                .unwrap()
                .keyword,
            "!join"
        );
        // Un mot-clé avec un symbole ou trop long n'est pas reconnu.
        assert!(parse("Raffle ! 20 secondes restantes. Go \"!jo;in\"").is_none());
        let long = format!("Raffle ! 20 secondes restantes. Go \"!{}\"", "a".repeat(21));
        assert!(parse(&long).is_none());
    }

    #[test]
    fn autres_messages_du_bot_ignores() {
        // Fin de raffle et gagnants : pas de compte à rebours, donc aucune réponse.
        assert!(parse("Raffle terminée ! Le gagnant est @someone, félicitations").is_none());
        assert!(parse("@someone a gagné 777 DOLLEX à la raffle, bravo !").is_none());
        assert!(parse("Raffle pour 1111 BOOSTEX ! Go \"!join\"").is_none());
    }
}
