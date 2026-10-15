//! Parsing Twitch's IRC lines (IRCv3 format with "tags").
//!
//! Example of a received line:
//! `@color=#FF0000;display-name=Bob;emotes=25:0-4 :bob!bob@bob.tmi.twitch.tv PRIVMSG #somechannel :Kappa salut`

use std::collections::HashMap;

pub struct IrcMessage {
    pub tags: HashMap<String, String>,
    /// Sender's nickname (`bob` in `bob!bob@bob.tmi.twitch.tv`).
    pub nick: Option<String>,
    pub command: String,
    pub params: Vec<String>,
}

impl IrcMessage {
    pub fn tag(&self, key: &str) -> &str {
        self.tags.get(key).map_or("", String::as_str)
    }

    /// Last parameter (the message text, in general).
    pub fn trailing(&self) -> &str {
        self.params.last().map_or("", String::as_str)
    }
}

pub fn parse(line: &str) -> Option<IrcMessage> {
    let mut rest = line.trim_end_matches(['\r', '\n']);

    let mut tags = HashMap::new();
    if let Some(stripped) = rest.strip_prefix('@') {
        let (raw_tags, after) = stripped.split_once(' ')?;
        for pair in raw_tags.split(';') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            tags.insert(key.to_owned(), unescape_tag(value));
        }
        rest = after.trim_start();
    }

    let mut nick = None;
    if let Some(stripped) = rest.strip_prefix(':') {
        let (prefix, after) = stripped.split_once(' ')?;
        nick = prefix.split('!').next().map(str::to_owned);
        rest = after.trim_start();
    }

    let (command, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
    if command.is_empty() {
        return None;
    }
    let mut params = Vec::new();
    loop {
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() {
            break;
        }
        if let Some(trailing) = rest.strip_prefix(':') {
            params.push(trailing.to_owned());
            break;
        }
        let (param, after) = rest.split_once(' ').unwrap_or((rest, ""));
        params.push(param.to_owned());
        rest = after;
    }

    Some(IrcMessage {
        tags,
        nick,
        command: command.to_owned(),
        params,
    })
}

/// Tag values escape some characters (`\s` = space, `\:` = `;`…).
fn unescape_tag(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some(':') => out.push(';'),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privmsg_avec_tags() {
        let m = parse(
            "@badges=subscriber/12;color=#FF0000;display-name=Bob\\sB;emotes=25:0-4 :bob!bob@bob.tmi.twitch.tv PRIVMSG #somechannel :Kappa salut : ça va",
        )
        .unwrap();
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.nick.as_deref(), Some("bob"));
        assert_eq!(m.tag("display-name"), "Bob B");
        assert_eq!(m.tag("emotes"), "25:0-4");
        assert_eq!(m.params, ["#somechannel", "Kappa salut : ça va"]);
    }

    #[test]
    fn ping_et_lignes_invalides() {
        let m = parse("PING :tmi.twitch.tv").unwrap();
        assert_eq!(m.command, "PING");
        assert_eq!(m.trailing(), "tmi.twitch.tv");
        assert!(parse("").is_none());
        assert!(parse("@a=b").is_none());
    }
}
