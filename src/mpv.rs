//! Launching mpv and driving it through its IPC interface: JSON commands
//! exchanged over a Unix socket (a local communication "file").

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Where mpv displays the video.
pub enum Placement {
    /// In the X11 window whose identifier is given (embedded video).
    Embedded(u32),
    /// In its own window.
    Detached,
}

#[derive(Debug)]
pub enum MpvEvent {
    /// IPC connection established: write channel to send commands.
    Connected(UnixStream),
    Volume(f64),
    /// Sound muted or restored.
    Mute(bool),
    /// End of playback: `"eof"` (stream ended), `"quit"` (window closed), `"error"`…
    EndFile(String),
}

/// Method used to display the video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// X11 "XVideo" interface: very light on memory, decoding by the processor.
    Xv,
    /// OpenGL/EGL through the graphics card.
    Gpu {
        hardware_decoding: bool,
        /// Only load the NVIDIA EGL driver: avoids Mesa (and its 40 MB
        /// of LLVM) being loaded for nothing when the desktop runs on the NVIDIA.
        nvidia_only: bool,
    },
}

/// Declaration file of the NVIDIA EGL driver (glvnd library).
pub const NVIDIA_EGL_VENDOR: &str = "/usr/share/glvnd/egl_vendor.d/10_nvidia.json";

pub struct Options<'a> {
    pub program: &'a Path,
    pub url: &'a str,
    pub placement: Placement,
    pub output: Output,
    pub volume: u8,
    pub socket: PathBuf,
}

/// A mpv process in progress, closed automatically when the value is dropped.
pub struct Mpv {
    child: Child,
    socket: PathBuf,
    ipc: Option<UnixStream>,
    output: Output,
}

impl Mpv {
    pub fn spawn(
        options: Options,
        on_event: impl Fn(MpvEvent) + Send + 'static,
    ) -> Result<Mpv, String> {
        // mpv must only read streamlink's local server.
        if !options.url.starts_with("http://127.0.0.1:") {
            return Err(format!("adresse de flux refusée : {}", options.url));
        }
        let _ = fs::remove_file(&options.socket);

        let mut ipc_arg = OsString::from("--input-ipc-server=");
        ipc_arg.push(&options.socket);

        let mut command = Command::new(options.program);
        command
            // Security and reproducibility: no user config, no scripts, no youtube-dl.
            .args([
                "--no-config",
                "--load-scripts=no",
                "--ytdl=no",
                "--no-terminal",
            ])
            .arg(ipc_arg)
            .args(["--force-window=immediate", "--keep-open=no", "--idle=no"])
            .args(["--volume-max=100", &format!("--volume={}", options.volume)])
            // Caps the stream buffer in memory (mpv's default: 150 + 50 MiB).
            // In low latency, it only holds ~3 s of video (~3 MiB at 1080p60);
            // measured: 32 MiB made mpv climb to ~240 MB, 8 MiB keeps it stable at ~197 MB.
            .args(["--demuxer-max-bytes=8MiB", "--demuxer-max-back-bytes=1MiB"]);
        match options.placement {
            Placement::Embedded(window) => {
                command.arg(format!("--wid={window}"));
            }
            Placement::Detached => {
                command.args(["--title=Evolve TV — vidéo", "--geometry=960x540"]);
            }
        }
        match options.output {
            Output::Xv => {
                command.args(["--vo=xv", "--hwdec=no"]);
            }
            Output::Gpu {
                hardware_decoding,
                nvidia_only,
            } => {
                command.args([
                    "--vo=gpu",
                    if hardware_decoding {
                        "--hwdec=auto-safe"
                    } else {
                        "--hwdec=no"
                    },
                ]);
                if nvidia_only {
                    command.env("__EGL_VENDOR_LIBRARY_FILENAMES", NVIDIA_EGL_VENDOR);
                }
            }
        }
        let child = command
            .arg("--")
            .arg(options.url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("lancement de mpv : {e}"))?;

        let socket = options.socket.clone();
        thread::spawn(move || ipc_loop(&socket, on_event));
        Ok(Mpv {
            child,
            socket: options.socket,
            ipc: None,
            output: options.output,
        })
    }

    pub fn output(&self) -> Output {
        self.output
    }

    pub fn set_ipc(&mut self, stream: UnixStream) {
        self.ipc = Some(stream);
    }

    /// Sends a command, for example `json!(["cycle", "pause"])`.
    pub fn command(&mut self, command: Value) {
        if let Some(ipc) = &mut self.ipc
            && send(ipc, command).is_err()
        {
            self.ipc = None;
        }
    }

    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // We ask mpv politely to quit, then force it if it lingers.
        self.command(json!(["quit"]));
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
    }
}

fn send(stream: &mut UnixStream, command: Value) -> io::Result<()> {
    let mut line = json!({ "command": command }).to_string();
    line.push('\n');
    stream.write_all(line.as_bytes())
}

/// Thread that connects to mpv's socket and forwards its events.
fn ipc_loop(socket: &Path, on_event: impl Fn(MpvEvent)) {
    // mpv takes a short moment to create the socket: we retry for 10 s.
    let deadline = Instant::now() + Duration::from_secs(10);
    let stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            Err(_) => return,
        }
    };
    // `try_clone` creates a second access to the same socket: one to read here,
    // one to write from the interface.
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let _ = writer.set_write_timeout(Some(Duration::from_secs(1)));
    if send(&mut writer, json!(["observe_property", 1, "volume"])).is_err()
        || send(&mut writer, json!(["observe_property", 2, "mute"])).is_err()
    {
        return;
    }
    on_event(MpvEvent::Connected(writer));

    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message["event"].as_str() {
            Some("property-change") if message["name"] == "volume" => {
                if let Some(volume) = message["data"].as_f64() {
                    on_event(MpvEvent::Volume(volume));
                }
            }
            Some("property-change") if message["name"] == "mute" => {
                if let Some(muted) = message["data"].as_bool() {
                    on_event(MpvEvent::Mute(muted));
                }
            }
            Some("end-file") => {
                let reason = message["reason"].as_str().unwrap_or("unknown");
                on_event(MpvEvent::EndFile(reason.into()));
            }
            _ => {}
        }
    }
}
