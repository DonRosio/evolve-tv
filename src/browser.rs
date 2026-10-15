//! Opening web addresses in the default browser (through `xdg-open`).

use std::process::{Command, Stdio};
use std::thread;

use crate::paths;

/// Acceptable web address: `http(s)://`, without spaces or control characters.
pub fn is_web_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url.len() > "https://".len()
        && url.len() <= 2048
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Real domain of an address, shown before opening a link.
/// `https://twitch.tv@trap.com/x` → `trap.com` (everything before `@` is ignored
/// by the browser: a classic booby-trapped link trick).
pub fn domain(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.rsplit('@').next().unwrap_or(authority)
}

/// Opens the address in the browser. Non-web addresses are ignored.
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
        // Collects the exit status so as not to leave a "zombie" process.
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
