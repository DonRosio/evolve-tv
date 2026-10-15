//! Driving streamlink: it connects to Twitch and serves the video stream
//! on `127.0.0.1` (reachable only from this PC), where mpv comes to read it.

use std::cmp::Reverse;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;

use crate::config;

/// Messages reported by streamlink while it runs.
#[derive(Debug)]
pub enum ServerEvent {
    /// The channel is offline: streamlink retries every 30 s.
    WaitingForLive,
    /// Qualities offered by Twitch, from best to worst.
    Qualities(Vec<String>),
    /// The local server is ready: mpv can read this address.
    Ready(String),
    /// A player connected and streamlink sends it this quality.
    Opened(String),
    /// Message d'erreur de streamlink.
    Error(String),
}

/// Image heights used by Twitch, from largest to smallest.
const HEIGHTS: [u32; 8] = [1440, 1080, 936, 900, 720, 480, 360, 160];

/// `"1080p60"` → `(1080, Some(60))` ; `"480p"` → `(480, None)`.
pub fn parse_resolution(name: &str) -> Option<(u32, Option<u32>)> {
    let (height, fps) = name.split_once('p')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(height) || !(fps.is_empty() || digits(fps)) {
        return None;
    }
    let height: u32 = height.parse().ok().filter(|h| (1..=4320).contains(h))?;
    let fps = match fps {
        "" => None,
        fps => Some(fps.parse().ok().filter(|f| (1..=240).contains(f))?),
    };
    Some((height, fps))
}

/// Ordered list of quality names to ask streamlink for, to get
/// "the best quality without exceeding `max_quality`". streamlink takes the
/// first available name, hence an automatic fallback (480p → 360p → …).
/// The `_portrait` variants (phones) are never requested.
pub fn candidates_up_to(max_quality: &str) -> Vec<String> {
    let Some((max_height, max_fps)) = parse_resolution(max_quality) else {
        return vec!["best".into()];
    };
    let mut heights: Vec<u32> = HEIGHTS.into_iter().filter(|&h| h <= max_height).collect();
    if !heights.contains(&max_height) {
        heights.insert(0, max_height);
    }
    let mut names = Vec::new();
    for height in heights {
        for fps in [60, 30] {
            if height == max_height && max_fps.is_some_and(|max| fps > max) {
                continue;
            }
            names.push(format!("{height}p{fps}"));
        }
        // Old Twitch naming, without a frame rate.
        names.push(format!("{height}p"));
    }
    // Last resort if no standard name exists.
    names.push(if max_height >= 720 { "best" } else { "worst" }.into());
    names
}

/// Predicts the quality streamlink will pick: the first name of `candidates`
/// present in `available` (sorted from best to worst).
pub fn expected_choice(candidates: &[String], available: &[String]) -> Option<String> {
    let video = || available.iter().filter(|q| parse_resolution(q).is_some());
    candidates
        .iter()
        .find_map(|candidate| match candidate.as_str() {
            "best" => video().next().cloned(),
            "worst" => video().next_back().cloned(),
            name => available.iter().find(|q| *q == name).cloned(),
        })
}

pub fn channel_url(channel: &str) -> String {
    format!("https://www.twitch.tv/{channel}")
}

/// A streamlink process in progress. It is stopped automatically when this
/// value is dropped (see `impl Drop` below).
pub struct Server {
    child: Child,
}

impl Server {
    /// Starts streamlink. It waits for the channel to be online, then opens the local server.
    /// `on_event` is called from background threads at every useful message.
    pub fn start(
        program: &Path,
        channel: &str,
        qualities: &[String],
        on_event: impl Fn(ServerEvent) + Clone + Send + 'static,
    ) -> Result<Server, String> {
        // Defence in depth: these values become command arguments.
        if !config::is_valid_channel(channel) {
            return Err(format!("nom de chaîne invalide : {channel}"));
        }
        if qualities.is_empty() || !qualities.iter().all(|q| config::is_valid_quality(q)) {
            return Err("liste de qualités invalide".into());
        }

        // No shell (`sh -c`): each argument is passed as is, nothing can
        // be interpreted as a command.
        let mut child = Command::new(program)
            // Ignores ~/.config/streamlink/config: only our code decides the behaviour.
            .arg("--no-config")
            .args(["--loglevel", "info"])
            .arg("--twitch-low-latency")
            .args([
                "--player-external-http",
                "--player-external-http-continuous",
                "yes",
            ])
            .args(["--player-external-http-interface", "127.0.0.1"])
            // Offline: retry every 30 s, indefinitely.
            .args(["--retry-streams", "30", "--retry-max", "0"])
            // `--`: everything that follows is a value, never an option.
            .arg("--")
            .arg(channel_url(channel))
            .arg(qualities.join(","))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("lancement de streamlink : {e}"))?;

        if let Some(stdout) = child.stdout.take() {
            read_lines(stdout, on_event.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            read_lines(stderr, on_event);
        }
        Ok(Server { child })
    }

    /// `Some(status)` if streamlink stopped.
    pub fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // `wait` collects the exit status: without it, a "zombie" process would remain.
        let _ = self.child.wait();
    }
}

/// Reads streamlink's output line by line in a dedicated thread.
fn read_lines(pipe: impl Read + Send + 'static, on_event: impl Fn(ServerEvent) + Send + 'static) {
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            if let Some(event) = parse_line(&line) {
                on_event(event);
            }
        }
    });
}

fn parse_line(line: &str) -> Option<ServerEvent> {
    let line = line.trim();
    if let Some(list) = after(line, "Available streams: ") {
        return Some(ServerEvent::Qualities(parse_available(list)));
    }
    if line.contains("Waiting for streams") {
        return Some(ServerEvent::WaitingForLive);
    }
    if let Some(rest) = after(line, "Opening stream: ") {
        let name = rest.split(' ').next().unwrap_or_default();
        return config::is_valid_quality(name).then(|| ServerEvent::Opened(name.into()));
    }
    if let Some(url) =
        after(line, "http://127.0.0.1:").map(|port| format!("http://127.0.0.1:{port}"))
    {
        return is_local_url(&url).then_some(ServerEvent::Ready(url));
    }
    let error = line.strip_prefix("error: ").or_else(|| {
        line.contains("][error]")
            .then(|| after(line, "] ").unwrap_or(line))
    });
    error.map(|msg| ServerEvent::Error(msg.chars().take(300).collect()))
}

/// Text located after `marker` in `line`.
fn after<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    line.find(marker).map(|i| &line[i + marker.len()..])
}

/// Only accepts `http://127.0.0.1:<port>/`: mpv will never read another address.
fn is_local_url(url: &str) -> bool {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.strip_suffix('/'))
        .is_some_and(|port| port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok())
}

/// `"audio_only, 160p30 (worst), 480p30, 1080p60 (best)"` → `["1080p60", "480p30", "160p30", "audio_only"]`
fn parse_available(list: &str) -> Vec<String> {
    let mut names: Vec<String> = list
        .split(", ")
        .filter_map(|item| item.split(' ').next())
        .filter(|name| config::is_valid_quality(name) && !name.ends_with("_portrait"))
        .map(String::from)
        .collect();
    names.sort_by_key(|name| {
        Reverse(parse_resolution(name).map_or((0, 0), |(h, fps)| (h, fps.unwrap_or(30))))
    });
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution() {
        assert_eq!(parse_resolution("1080p60"), Some((1080, Some(60))));
        assert_eq!(parse_resolution("480p"), Some((480, None)));
        assert_eq!(parse_resolution("480p30_portrait"), None);
        assert_eq!(parse_resolution("audio_only"), None);
        assert_eq!(parse_resolution("+480p"), None);
    }

    #[test]
    fn candidats_5g() {
        let c = candidates_up_to("480p");
        assert_eq!(&c[..4], ["480p60", "480p30", "480p", "360p60"]);
        assert_eq!(c.last().unwrap(), "worst");
        assert!(!c.iter().any(|q| q.starts_with("720")));
    }

    #[test]
    fn candidats_wifi() {
        let c = candidates_up_to("1080p30");
        assert_eq!(&c[..3], ["1080p30", "1080p", "936p60"]);
        assert_eq!(c.last().unwrap(), "best");
    }

    #[test]
    fn prevision_du_choix() {
        let available: Vec<String> = [
            "1080p60",
            "720p60",
            "480p30",
            "360p30",
            "160p30",
            "audio_only",
        ]
        .map(String::from)
        .into();
        let choice = |max| expected_choice(&candidates_up_to(max), &available);
        assert_eq!(choice("480p").as_deref(), Some("480p30"));
        assert_eq!(choice("1080p60").as_deref(), Some("1080p60"));
        assert_eq!(choice("720p30").as_deref(), Some("480p30"));
        assert_eq!(choice("100p").as_deref(), Some("160p30")); // repli « worst »
        assert_eq!(expected_choice(&candidates_up_to("480p"), &[]), None);
    }

    #[test]
    fn lignes_streamlink() {
        let line = "[cli][info] Available streams: audio_only, 284p30_portrait (worst), 160p30, 480p30, 1080p60 (best)";
        let Some(ServerEvent::Qualities(q)) = parse_line(line) else {
            panic!()
        };
        assert_eq!(q, ["1080p60", "480p30", "160p30", "audio_only"]);

        let Some(ServerEvent::Ready(url)) = parse_line("[cli][info]  http://127.0.0.1:39019/")
        else {
            panic!()
        };
        assert_eq!(url, "http://127.0.0.1:39019/");
        assert!(parse_line("[cli][info]  http://127.0.0.1:39019/../evil").is_none());

        let Some(ServerEvent::Opened(q)) = parse_line("[cli][info] Opening stream: 360p30 (hls)")
        else {
            panic!()
        };
        assert_eq!(q, "360p30");

        assert!(matches!(
            parse_line("[cli][info] Waiting for streams, retrying every 30.0 second(s)"),
            Some(ServerEvent::WaitingForLive)
        ));
        assert!(matches!(
            parse_line("error: No playable streams found"),
            Some(ServerEvent::Error(_))
        ));
    }
}
