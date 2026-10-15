//! Detecting the current Wi-Fi network through `nmcli`, NetworkManager's command-line tool.

use std::process::{Command, Stdio};

use crate::paths;

/// Name (SSID) of the active Wi-Fi network, or `None` (no Wi-Fi, cable, nmcli missing…).
/// Blocking call (a few tens of ms): to be done outside the interface thread.
pub fn current_wifi() -> Option<String> {
    let nmcli = paths::find_program("nmcli", None)?;
    let output = Command::new(nmcli)
        // -t: "terse" output, easy to parse; --rescan no: no new scan (fast).
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
        // Forces English: otherwise the ACTIVE column is "oui" instead of "yes".
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

/// In `-t` mode, nmcli escapes `:` and `\` with a backslash: we remove those escapes.
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
