//! Fenêtre X11 « enfant » posée dans la fenêtre egui : mpv y dessine la vidéo (`--wid`).
//!
//! Sous X11, une fenêtre peut en contenir d'autres. On crée un rectangle noir à
//! l'intérieur de notre fenêtre, on donne son identifiant à mpv, et on le déplace
//! ou le redimensionne pour qu'il suive la zone vidéo de l'interface.

use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt, CreateWindowAux, WindowClass};
use x11rb::rust_connection::RustConnection;

pub struct VideoSurface {
    conn: RustConnection,
    window: u32,
    geometry: Option<(i32, i32, u32, u32)>,
    visible: bool,
}

fn err(e: impl std::fmt::Display) -> String {
    format!("X11 : {e}")
}

impl VideoSurface {
    /// Crée la fenêtre enfant (cachée) dans la fenêtre `parent`.
    pub fn new(parent: u32) -> Result<VideoSurface, String> {
        let (conn, screen) = x11rb::connect(None).map_err(err)?;
        let black = conn
            .setup()
            .roots
            .get(screen)
            .ok_or("écran X11 introuvable")?
            .black_pixel;
        let window = conn.generate_id().map_err(err)?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            parent,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(black),
        )
        .map_err(err)?;
        conn.flush().map_err(err)?;
        Ok(VideoSurface {
            conn,
            window,
            geometry: None,
            visible: false,
        })
    }

    /// Identifiant X11 de la fenêtre, à transmettre à mpv.
    pub fn id(&self) -> u32 {
        self.window
    }

    /// Place la fenêtre (en pixels physiques, relativement à la fenêtre principale) et l'affiche.
    pub fn show_at(&mut self, x: i32, y: i32, width: u32, height: u32) -> Result<(), String> {
        let geometry = (x, y, width.max(1), height.max(1));
        if self.geometry != Some(geometry) {
            let aux = ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .width(geometry.2)
                .height(geometry.3);
            self.conn.configure_window(self.window, &aux).map_err(err)?;
            self.geometry = Some(geometry);
        }
        if !self.visible {
            self.conn.map_window(self.window).map_err(err)?;
            self.visible = true;
        }
        self.flush()
    }

    pub fn hide(&mut self) -> Result<(), String> {
        if self.visible {
            self.conn.unmap_window(self.window).map_err(err)?;
            self.visible = false;
        }
        self.flush()
    }

    fn flush(&self) -> Result<(), String> {
        self.conn.flush().map_err(err)?;
        // Vide la file des événements/erreurs X11 qu'on n'utilise pas, pour qu'elle
        // ne grossisse pas indéfiniment.
        while let Ok(Some(_)) = self.conn.poll_for_event() {}
        Ok(())
    }
}

impl Drop for VideoSurface {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}
