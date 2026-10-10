//! Détection du réseau Wi-Fi actuel via `nmcli`, l'outil en ligne de commande de NetworkManager.

use std::process::{Command, Stdio};

use crate::paths;

/// Nom (SSID) du réseau Wi-Fi actif, ou `None` (pas de Wi-Fi, câble, nmcli absent…).
/// Appel bloquant (quelques dizaines de ms) : à faire hors du thread de l'interface.
pub fn current_wifi() -> Option<String> {
    let nmcli = paths::find_program("nmcli", None)?;
    let output = Command::new(nmcli)
        // -t : sortie « terse », facile à analyser ; --rescan no : pas de nouveau scan (rapide).
        .args([
            "-t",
            "-f",
            "ACTIVE,SSID",
            "device",
            "wifi",
            "list",
            "--rescan",
            "no",
        ])
        // Force l'anglais : sinon la colonne ACTIVE vaut « oui » au lieu de « yes ».
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("yes:"))
        .map(unescape)
        .filter(|ssid| !ssid.is_empty())
}

/// En mode `-t`, nmcli échappe `:` et `\` avec un antislash : on retire ces échappements.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.extend(chars.next());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::unescape;

    #[test]
    fn unescape_retire_les_antislashs() {
        assert_eq!(unescape(r"Box\:Maison"), "Box:Maison");
        assert_eq!(unescape(r"a\\b"), r"a\b");
    }
}
