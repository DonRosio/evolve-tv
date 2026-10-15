//! Connection to the Twitch chat (IRC encrypted with TLS) in a dedicated thread,
//! with automatic reconnection and respect for the sending limits.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::irc::{self, IrcMessage};

const HOST: &str = "irc.chat.twitch.tv";
const PORT: u16 = 6697;
/// Twitch refuses messages of more than 500 characters.
const MAX_MESSAGE_CHARS: usize = 500;
const MAX_LINE: usize = 64 * 1024;
/// Added every other time to a message identical to the previous one (less than 30 s),
/// which Twitch would refuse otherwise. Same trick as Chatterino: space + U+E0000,
/// an invisible character.
const INVISIBLE_SUFFIX: &str = " \u{E0000}";

/// Credentials used at every (re)connection. Shared with the application,
/// which updates them when the token is renewed. Deliberately without `Debug`.
pub struct ChatLogin {
    pub login: String,
    pub access_token: String,
}

pub type SharedLogin = Arc<Mutex<Option<ChatLogin>>>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Badge {
    pub set: String,
    pub version: String,
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub id: String,
    pub login: String,
    pub display_name: String,
    pub color: Option<[u8; 3]>,
    pub badges: Vec<Badge>,
    pub text: String,
    /// Twitch emotes: (identifier, first character, last character).
    pub emotes: Vec<(String, usize, usize)>,
    /// `/me` message (shown in colour).
    pub action: bool,
    /// Sent by me (Twitch does not send our own messages back).
    pub own: bool,
    /// This person's first message in the channel.
    pub first: bool,
    /// Reply to another message.
    pub reply: Option<ReplyInfo>,
}

#[derive(Clone, Debug)]
pub struct ReplyInfo {
    pub parent_name: String,
    pub parent_body: String,
}

/// Message being replied to.
#[derive(Clone, Debug)]
pub struct ReplyTarget {
    pub id: String,
    pub name: String,
    pub body: String,
}

pub enum ClearTarget {
    Message(String),
    User(String),
    All,
}

pub enum ChatEvent {
    Connected {
        logged_in: bool,
    },
    Disconnected(String),
    Message(ChatMessage),
    /// System message (subscription, raid, announcement…).
    Notice(String),
    Clear(ClearTarget),
    /// Numeric identifier of the channel (used to fetch its badges).
    RoomId(String),
    /// Twitch refuses the token: the application must renew it.
    AuthFailed,
    /// Message not sent (limit, slow mode, chat disconnected…).
    SendRejected(String),
}

enum Command {
    Send {
        text: String,
        reply_to: Option<ReplyTarget>,
    },
}

/// Handle on the chat thread. Dropping it stops the connection.
pub struct Chat {
    commands: Sender<Command>,
    stop: Arc<AtomicBool>,
}

impl Chat {
    pub fn spawn(
        channel: String,
        login: SharedLogin,
        on_event: impl Fn(ChatEvent) + Send + 'static,
    ) -> Chat {
        let (commands, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        thread::spawn(move || run(&channel, &login, &receiver, &stop_thread, &on_event));
        Chat { commands, stop }
    }

    pub fn send(&self, text: &str, reply_to: Option<ReplyTarget>) {
        let _ = self.commands.send(Command::Send {
            text: text.to_owned(),
            reply_to,
        });
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        // The thread checks this flag 10 times a second and ends.
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Boucle de reconnexion : attend 1 s, 2 s, 4 s… (60 s maximum) entre deux essais.
fn run(
    channel: &str,
    login: &SharedLogin,
    commands: &Receiver<Command>,
    stop: &AtomicBool,
    on_event: &dyn Fn(ChatEvent),
) {
    let mut delay = Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let Err(reason) = session(channel, login, commands, stop, on_event) else {
            return; // stop requested
        };
        if stop.load(Ordering::Relaxed) {
            return;
        }
        on_event(ChatEvent::Disconnected(reason));
        if started.elapsed() > Duration::from_secs(60) {
            delay = Duration::from_secs(1);
        }
        let deadline = Instant::now() + delay;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            // During the interruption, messages to send are refused rather than
            // being sent late.
            while let Ok(Command::Send { .. }) = commands.try_recv() {
                on_event(ChatEvent::SendRejected(
                    "chat déconnecté, message non envoyé".into(),
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
        delay = (delay * 2).min(Duration::from_secs(60));
    }
}

type Stream = StreamOwned<ClientConnection, TcpStream>;

/// Configuration TLS : certificats racine de Mozilla, cryptographie `ring`.
fn tls_config() -> Result<Arc<ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .with_root_certificates(roots)
                .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}

fn connect() -> Result<Stream, String> {
    let addresses = (HOST, PORT)
        .to_socket_addrs()
        .map_err(|e| format!("adresse de {HOST} introuvable : {e}"))?;
    let mut last_error = String::from("aucune adresse");
    for address in addresses {
        match TcpStream::connect_timeout(&address, Duration::from_secs(10)) {
            Ok(mut tcp) => {
                let timeout = |tcp: &TcpStream, read: Duration| {
                    tcp.set_read_timeout(Some(read))
                        .and_then(|()| tcp.set_write_timeout(Some(Duration::from_secs(10))))
                        .map_err(|e| e.to_string())
                };
                // The TLS "handshake" (key exchange with the server) is done
                // first, with a generous timeout…
                timeout(&tcp, Duration::from_secs(10))?;
                let name = ServerName::try_from(HOST).map_err(|e| e.to_string())?;
                let mut tls =
                    ClientConnection::new(tls_config()?, name).map_err(|e| e.to_string())?;
                while tls.is_handshaking() {
                    tls.complete_io(&mut tcp)
                        .map_err(|e| format!("échec de la connexion chiffrée : {e}"))?;
                }
                // … then reading with a short timeout: the thread can thus also send
                // messages and check whether it must stop.
                timeout(&tcp, Duration::from_millis(100))?;
                return Ok(StreamOwned::new(tls, tcp));
            }
            Err(e) => last_error = e.to_string(),
        }
    }
    Err(format!("connexion à {HOST} impossible : {last_error}"))
}

fn send_line(stream: &mut Stream, line: &str) -> Result<(), String> {
    stream
        .write_all(format!("{line}\r\n").as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| format!("envoi impossible : {e}"))
}

/// What Twitch tells us about ourselves (USERSTATE): used to show our messages.
#[derive(Default)]
struct Me {
    login: Option<String>,
    display_name: String,
    color: Option<[u8; 3]>,
    badges: Vec<Badge>,
}

impl Me {
    /// Moderator, VIP or channel owner: wider sending limits.
    fn privileged(&self) -> bool {
        self.badges
            .iter()
            .any(|b| matches!(b.set.as_str(), "broadcaster" | "moderator" | "vip"))
    }
}

/// Twitch's sending limits, checked before every message to avoid
/// being blocked temporarily.
#[derive(Default)]
struct SendRules {
    recent: VecDeque<Instant>,
    slow_mode_secs: u64,
    last_sent: Option<Instant>,
    /// Last text sent and whether the invisible suffix was added.
    last_text: Option<String>,
    suffix_used: bool,
}

impl SendRules {
    fn check(&mut self, privileged: bool) -> Result<(), String> {
        let now = Instant::now();
        let window = Duration::from_secs(30);
        while self.recent.front().is_some_and(|t| now - *t > window) {
            self.recent.pop_front();
        }
        let limit = if privileged { 100 } else { 20 };
        if self.recent.len() >= limit {
            let wait = window.saturating_sub(now - self.recent[0]).as_secs() + 1;
            return Err(format!(
                "limite de Twitch ({limit} messages / 30 s) : réessaie dans {wait} s"
            ));
        }
        if !privileged
            && self.slow_mode_secs > 0
            && let Some(last) = self.last_sent
        {
            let slow = Duration::from_secs(self.slow_mode_secs);
            if now - last < slow {
                let wait = (slow - (now - last)).as_secs() + 1;
                return Err(format!("mode lent : encore {wait} s"));
            }
        }
        Ok(())
    }

    /// Should the invisible suffix be added? Yes if this text is identical to the
    /// previous one, sent less than 30 s ago… and in alternation: the next
    /// identical one will go out without a suffix (it then differs from the previous message).
    fn needs_suffix(&self, text: &str) -> bool {
        self.last_text.as_deref() == Some(text)
            && self
                .last_sent
                .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
            && !self.suffix_used
    }

    fn record(&mut self, text: &str, suffix_used: bool) {
        let now = Instant::now();
        self.recent.push_back(now);
        self.last_sent = Some(now);
        self.last_text = Some(text.to_owned());
        self.suffix_used = suffix_used;
    }
}

/// A complete connection. `Ok(())`: stop requested; `Err`: connection lost.
fn session(
    channel: &str,
    login: &SharedLogin,
    commands: &Receiver<Command>,
    stop: &AtomicBool,
    on_event: &dyn Fn(ChatEvent),
) -> Result<(), String> {
    let credentials = login
        .lock()
        .map_err(|_| "verrou empoisonné")?
        .as_ref()
        .map(|l| (l.login.clone(), l.access_token.clone()));
    let mut stream = connect()?;
    let mut me = Me::default();
    let mut rules = SendRules::default();

    send_line(&mut stream, "CAP REQ :twitch.tv/tags twitch.tv/commands")?;
    match &credentials {
        Some((login, token)) => {
            send_line(&mut stream, &format!("PASS oauth:{token}"))?;
            send_line(&mut stream, &format!("NICK {login}"))?;
            me.login = Some(login.clone());
            me.display_name = login.clone();
        }
        None => {
            // Anonymous read-only connection, allowed by Twitch.
            let n = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos() % 90_000 + 10_000);
            send_line(&mut stream, &format!("NICK justinfan{n}"))?;
        }
    }
    send_line(&mut stream, &format!("JOIN #{channel}"))?;

    let mut pending: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut last_received = Instant::now();
    let mut ping_sent = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = send_line(&mut stream, &format!("PART #{channel}"));
            return Ok(());
        }
        while let Ok(Command::Send { text, reply_to }) = commands.try_recv() {
            let reply_to = reply_to.as_ref();
            send_message(
                &mut stream,
                channel,
                &text,
                reply_to,
                &mut me,
                &mut rules,
                on_event,
            )?;
        }

        match stream.read(&mut buffer) {
            Ok(0) => return Err("connexion fermée par Twitch".into()),
            Ok(n) => {
                last_received = Instant::now();
                ping_sent = false;
                pending.extend_from_slice(&buffer[..n]);
                while let Some(pos) = pending.windows(2).position(|w| w == b"\r\n") {
                    let line: Vec<u8> = pending.drain(..pos + 2).collect();
                    let line = String::from_utf8_lossy(&line[..pos]);
                    if let Some(message) = irc::parse(&line) {
                        handle(
                            &mut stream,
                            &message,
                            &mut me,
                            &mut rules,
                            credentials.is_some(),
                            on_event,
                        )?;
                    }
                }
                if pending.len() > MAX_LINE {
                    return Err("ligne IRC trop longue".into());
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(format!("connexion perdue : {e}")),
        }

        // Twitch sends a PING every ~5 min. Without news, we check by ourselves.
        let silence = last_received.elapsed();
        if silence > Duration::from_secs(6 * 60) && !ping_sent {
            send_line(&mut stream, "PING :evolve-tv")?;
            ping_sent = true;
        }
        if silence > Duration::from_secs(6 * 60 + 20) {
            return Err("plus de réponse de Twitch".into());
        }
    }
}

fn send_message(
    stream: &mut Stream,
    channel: &str,
    text: &str,
    reply_to: Option<&ReplyTarget>,
    me: &mut Me,
    rules: &mut SendRules,
    on_event: &dyn Fn(ChatEvent),
) -> Result<(), String> {
    let Some(login) = me.login.clone() else {
        on_event(ChatEvent::SendRejected(
            "connecte-toi pour écrire dans le chat".into(),
        ));
        return Ok(());
    };
    let Some(text) = sanitize(text) else {
        return Ok(());
    };
    if let Err(reason) = rules.check(me.privileged()) {
        on_event(ChatEvent::SendRejected(reason));
        return Ok(());
    }
    let suffix = rules.needs_suffix(&text);
    let mut content = text.clone();
    if suffix {
        content.push_str(INVISIBLE_SUFFIX);
    }
    let (body, action) = match content.strip_prefix("/me ") {
        // Standard format of IRC actions (CTCP ACTION).
        Some(action) => (format!("\u{1}ACTION {action}\u{1}"), true),
        None => (content.clone(), false),
    };
    // Reply: the "reply-parent-msg-id" tag links the message to the original one.
    let reply_tag = match reply_to {
        Some(target) if is_valid_message_id(&target.id) => {
            format!("@reply-parent-msg-id={} ", target.id)
        }
        _ => String::new(),
    };
    send_line(stream, &format!("{reply_tag}PRIVMSG #{channel} :{body}"))?;
    rules.record(&text, suffix);
    let text = if action {
        text["/me ".len()..].to_owned()
    } else {
        text
    };
    on_event(ChatEvent::Message(ChatMessage {
        id: String::new(),
        display_name: me.display_name.clone(),
        login,
        color: me.color,
        badges: me.badges.clone(),
        text,
        emotes: Vec::new(),
        action,
        own: true,
        first: false,
        reply: reply_to.map(|target| ReplyInfo {
            parent_name: target.name.clone(),
            parent_body: target.body.clone(),
        }),
    }));
    Ok(())
}

/// Removes control characters (including line breaks, which would allow
/// injecting a second IRC command) and limits the length.
fn sanitize(text: &str) -> Option<String> {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.chars().take(MAX_MESSAGE_CHARS).collect())
}

fn handle(
    stream: &mut Stream,
    message: &IrcMessage,
    me: &mut Me,
    rules: &mut SendRules,
    logged_in: bool,
    on_event: &dyn Fn(ChatEvent),
) -> Result<(), String> {
    match message.command.as_str() {
        "PING" => send_line(stream, &format!("PONG :{}", message.trailing()))?,
        "001" => on_event(ChatEvent::Connected { logged_in }),
        "RECONNECT" => return Err("Twitch demande une reconnexion".into()),
        "PRIVMSG" => {
            if let Some(chat_message) = parse_privmsg(message) {
                on_event(ChatEvent::Message(chat_message));
            }
        }
        "USERNOTICE" => {
            let system = message.tag("system-msg");
            if !system.is_empty() {
                on_event(ChatEvent::Notice(system.to_owned()));
            }
            if message.params.len() > 1
                && let Some(chat_message) = parse_privmsg(message)
            {
                on_event(ChatEvent::Message(chat_message));
            }
        }
        "NOTICE" => {
            let text = message.trailing();
            if text.contains("Login authentication failed")
                || text.contains("Improperly formatted auth")
            {
                on_event(ChatEvent::AuthFailed);
                return Err("jeton refusé par Twitch".into());
            }
            let id = message.tag("msg-id");
            if id.starts_with("msg_") {
                on_event(ChatEvent::SendRejected(translate_refusal(id, text)));
            } else {
                on_event(ChatEvent::Notice(text.to_owned()));
            }
        }
        "CLEARCHAT" => {
            let target = match message.params.get(1) {
                Some(user) => ClearTarget::User(user.clone()),
                None => ClearTarget::All,
            };
            on_event(ChatEvent::Clear(target));
        }
        "CLEARMSG" => {
            let id = message.tag("target-msg-id");
            if !id.is_empty() {
                on_event(ChatEvent::Clear(ClearTarget::Message(id.to_owned())));
            }
        }
        "USERSTATE" | "GLOBALUSERSTATE" => {
            let name = message.tag("display-name");
            if !name.is_empty() {
                me.display_name = name.to_owned();
            }
            me.color = parse_color(message.tag("color"));
            if message.command == "USERSTATE" {
                me.badges = parse_badges(message.tag("badges"));
            }
        }
        "ROOMSTATE" => {
            // ROOMSTATE may only contain the settings that changed.
            if let Some(slow) = message.tags.get("slow") {
                rules.slow_mode_secs = slow.parse().unwrap_or(0);
            }
            let room = message.tag("room-id");
            if !room.is_empty() && room.bytes().all(|b| b.is_ascii_digit()) {
                on_event(ChatEvent::RoomId(room.to_owned()));
            }
        }
        _ => {}
    }
    Ok(())
}

fn translate_refusal(id: &str, original: &str) -> String {
    match id {
        "msg_duplicate" => "message refusé : identique au précédent (moins de 30 s)".into(),
        "msg_slowmode" => "message refusé : mode lent".into(),
        "msg_ratelimit" => "message refusé : envois trop rapides".into(),
        "msg_emoteonly" => "message refusé : chat en mode emotes uniquement".into(),
        "msg_subsonly" => "message refusé : chat réservé aux abonnés".into(),
        "msg_followersonly" | "msg_followersonly_zero" => {
            "message refusé : chat réservé aux followers".into()
        }
        _ => format!("message refusé : {original}"),
    }
}

fn parse_privmsg(message: &IrcMessage) -> Option<ChatMessage> {
    let login = message.nick.clone().or_else(|| {
        let login = message.tag("login");
        (!login.is_empty()).then(|| login.to_owned())
    })?;
    let mut text = message.trailing().to_owned();
    let mut action = false;
    if let Some(inner) = text
        .strip_prefix("\u{1}ACTION ")
        .and_then(|t| t.strip_suffix('\u{1}'))
    {
        text = inner.to_owned();
        action = true;
    }
    let display_name = match message.tag("display-name") {
        "" => login.clone(),
        name => name.to_owned(),
    };
    Some(ChatMessage {
        id: message.tag("id").to_owned(),
        login,
        display_name,
        color: parse_color(message.tag("color")),
        badges: parse_badges(message.tag("badges")),
        emotes: parse_emotes(message.tag("emotes")),
        text,
        action,
        own: false,
        first: message.tag("first-msg") == "1",
        reply: (!message.tag("reply-parent-msg-id").is_empty()).then(|| ReplyInfo {
            parent_name: match message.tag("reply-parent-display-name") {
                "" => message.tag("reply-parent-user-login").to_owned(),
                name => name.to_owned(),
            },
            parent_body: message.tag("reply-parent-msg-body").to_owned(),
        }),
    })
}

/// Twitch message identifier (UUID): hexadecimal digits and dashes.
pub fn is_valid_message_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// `"#1E90FF"` → `[30, 144, 255]`
fn parse_color(tag: &str) -> Option<[u8; 3]> {
    let hex = tag.strip_prefix('#').filter(|h| h.len() == 6)?;
    let channel = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// `"subscriber/12,premium/1"` → badges
fn parse_badges(tag: &str) -> Vec<Badge> {
    tag.split(',')
        .filter_map(|badge| badge.split_once('/'))
        .map(|(set, version)| Badge {
            set: set.to_owned(),
            version: version.to_owned(),
        })
        .collect()
}

/// `"25:0-4,12-16/1902:6-10"` → `[("25",0,4), ("25",12,16), ("1902",6,10)]`
fn parse_emotes(tag: &str) -> Vec<(String, usize, usize)> {
    let mut emotes = Vec::new();
    for group in tag.split('/') {
        let Some((id, ranges)) = group.split_once(':') else {
            continue;
        };
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        for range in ranges.split(',') {
            if let Some((start, end)) = range.split_once('-')
                && let (Ok(start), Ok(end)) = (start.parse(), end.parse())
                && start <= end
            {
                emotes.push((id.to_owned(), start, end));
            }
        }
    }
    emotes.sort_by_key(|e| e.1);
    emotes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyse_des_tags() {
        assert_eq!(parse_color("#1E90FF"), Some([30, 144, 255]));
        assert_eq!(parse_color(""), None);
        assert_eq!(parse_badges("subscriber/12,premium/1")[0].set, "subscriber");
        assert_eq!(
            parse_emotes("25:12-16,0-4/emotesv2_ab:6-10/a.b:1-2"),
            [
                ("25".to_owned(), 0, 4),
                ("emotesv2_ab".to_owned(), 6, 10),
                ("25".to_owned(), 12, 16)
            ]
        );
    }

    #[test]
    fn message_action() {
        let m = irc::parse(":bob!bob@bob PRIVMSG #somechannel :\u{1}ACTION danse\u{1}").unwrap();
        let chat = parse_privmsg(&m).unwrap();
        assert!(chat.action);
        assert_eq!(chat.text, "danse");
        assert_eq!(chat.display_name, "bob");
    }

    #[test]
    fn nettoyage_anti_injection() {
        assert_eq!(
            sanitize("salut\r\nPRIVMSG #autre :spam").unwrap(),
            "salut  PRIVMSG #autre :spam"
        );
        assert_eq!(sanitize("   "), None);
        assert_eq!(sanitize(&"a".repeat(600)).unwrap().len(), 500);
    }

    #[test]
    fn mode_lent() {
        let mut rules = SendRules {
            slow_mode_secs: 30,
            ..Default::default()
        };
        assert!(rules.check(false).is_ok());
        rules.record("a", false);
        assert!(rules.check(false).is_err());
        assert!(rules.check(true).is_ok()); // moderators are not concerned
    }

    #[test]
    fn contournement_anti_doublon() {
        let mut rules = SendRules::default();
        assert!(!rules.needs_suffix("!join")); // premier envoi : tel quel
        rules.record("!join", false);
        assert!(rules.needs_suffix("!join")); // doublon : suffixe invisible
        rules.record("!join", true);
        assert!(!rules.needs_suffix("!join")); // in alternation: without a suffix
        rules.record("!join", false);
        assert!(!rules.needs_suffix("autre chose"));
    }

    #[test]
    fn premier_message_et_reponse() {
        let m = irc::parse(
            "@first-msg=1;reply-parent-msg-id=abc-123;reply-parent-display-name=Alice;reply-parent-msg-body=salut\\sà\\stous :bob!bob@bob PRIVMSG #somechannel :@Alice coucou",
        )
        .unwrap();
        let chat = parse_privmsg(&m).unwrap();
        assert!(chat.first);
        let reply = chat.reply.unwrap();
        assert_eq!(reply.parent_name, "Alice");
        assert_eq!(reply.parent_body, "salut à tous");
        assert!(is_valid_message_id("885196de-cb67-427a-baa8-82f9b0fcd05f"));
        assert!(!is_valid_message_id("abc;PRIVMSG"));
    }
}
