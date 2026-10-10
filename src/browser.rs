//! Ouverture d'adresses web dans le navigateur par défaut (via `xdg-open`).

use std::process::{Command, Stdio};
use std::thread;

use crate::paths;

/// Adresse web acceptable : `http(s)://`, sans espace ni caractère de contrôle.
pub fn is_web_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url.len() > "https://".len()
        && url.len() <= 2048
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Domaine réel d'une adresse, affiché avant d'ouvrir un lien.
/// `https://twitch.tv@piege.com/x` → `piege.com` (tout ce qui précède `@` est ignoré
/// par le navigateur : une astuce classique de lien piégé).
pub fn domain(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.rsplit('@').next().unwrap_or(authority)
}

/// Ouvre l'adresse dans le navigateur. Les adresses non web sont ignorées.
pub fn open(url: &str) {
    if !is_web_url(url) {
        return;
    }
    let Some(xdg_open) = paths::find_program("xdg-open", None) else {
        return;
    };
    let child = Command::new(xdg_open)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        // Récupère le statut de fin pour ne pas laisser de processus « zombie ».
        thread::spawn(move || child.wait());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adresses() {
        assert!(is_web_url("https://www.twitch.tv/somechannel/clip/Abc-123"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("https://"));
        assert!(!is_web_url("https://a b"));
        assert_eq!(domain("https://www.twitch.tv/somechannel"), "www.twitch.tv");
        assert_eq!(domain("https://twitch.tv@piege.com/x"), "piege.com");
        assert_eq!(domain("http://exemple.fr?q=1"), "exemple.fr");
    }
}
