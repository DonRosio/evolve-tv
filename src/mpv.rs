//! Lancement de mpv et pilotage par son interface IPC : des commandes JSON
//! échangées sur un socket Unix (un « fichier » de communication local).

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Où mpv affiche la vidéo.
pub enum Placement {
    /// Dans la fenêtre X11 dont on donne l'identifiant (vidéo intégrée).
    Embedded(u32),
    /// Dans sa propre fenêtre.
    Detached,
}

#[derive(Debug)]
pub enum MpvEvent {
    /// Connexion IPC établie : canal d'écriture pour envoyer des commandes.
    Connected(UnixStream),
    Volume(f64),
    /// Fin de lecture : `"eof"` (flux terminé), `"quit"` (fenêtre fermée), `"error"`…
    EndFile(String),
}

/// Méthode d'affichage de la vidéo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Interface X11 « XVideo » : très légère en mémoire, décodage par le processeur.
    Xv,
    /// OpenGL/EGL via la carte graphique.
    Gpu {
        hardware_decoding: bool,
        /// Ne charger que le pilote EGL NVIDIA : évite que Mesa (et ses 40 Mo
        /// de LLVM) soit chargé pour rien quand le bureau tourne sur la NVIDIA.
        nvidia_only: bool,
    },
}

/// Fichier de déclaration du pilote EGL NVIDIA (bibliothèque glvnd).
pub const NVIDIA_EGL_VENDOR: &str = "/usr/share/glvnd/egl_vendor.d/10_nvidia.json";

pub struct Options<'a> {
    pub program: &'a Path,
    pub url: &'a str,
    pub placement: Placement,
    pub output: Output,
    pub volume: u8,
    pub socket: PathBuf,
}

/// Un processus mpv en cours, fermé automatiquement à la destruction de la valeur.
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
        // mpv ne doit lire que le serveur local de streamlink.
        if !options.url.starts_with("http://127.0.0.1:") {
            return Err(format!("adresse de flux refusée : {}", options.url));
        }
        let _ = fs::remove_file(&options.socket);

        let mut ipc_arg = OsString::from("--input-ipc-server=");
        ipc_arg.push(&options.socket);

        let mut command = Command::new(options.program);
        command
            // Sécurité et reproductibilité : ni config utilisateur, ni scripts, ni youtube-dl.
            .args([
                "--no-config",
                "--load-scripts=no",
                "--ytdl=no",
                "--no-terminal",
            ])
            .arg(ipc_arg)
            .args(["--force-window=immediate", "--keep-open=no", "--idle=no"])
            .args(["--volume-max=100", &format!("--volume={}", options.volume)])
            // Plafonne le tampon vidéo en mémoire : en direct, inutile de garder
            // des dizaines de Mo d'images déjà vues (défaut de mpv : 150 + 50 Mio).
            .args(["--demuxer-max-bytes=32MiB", "--demuxer-max-back-bytes=1MiB"]);
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

    /// Envoie une commande, par exemple `json!(["cycle", "pause"])`.
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
        // On demande poliment à mpv de quitter, puis on force s'il traîne.
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

/// Thread qui se connecte au socket de mpv et transmet ses événements.
fn ipc_loop(socket: &Path, on_event: impl Fn(MpvEvent)) {
    // mpv met un court instant à créer le socket : on réessaie pendant 10 s.
    let deadline = Instant::now() + Duration::from_secs(10);
    let stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            Err(_) => return,
        }
    };
    // `try_clone` crée un second accès au même socket : un pour lire ici,
    // un pour écrire depuis l'interface.
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let _ = writer.set_write_timeout(Some(Duration::from_secs(1)));
    if send(&mut writer, json!(["observe_property", 1, "volume"])).is_err() {
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
            Some("end-file") => {
                let reason = message["reason"].as_str().unwrap_or("unknown");
                on_event(MpvEvent::EndFile(reason.into()));
            }
            _ => {}
        }
    }
}
