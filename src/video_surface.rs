//! "Child" X11 window placed in the egui window: mpv draws the video in it (`--wid`).
//!
//! Under X11, a window can contain others. We create a black rectangle inside
//! our window, give its identifier to mpv, and move
//! or resize it so that it follows the interface's video area.

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
    /// Creates the child window (hidden) in the `parent` window.
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

    /// X11 identifier of the window, to hand to mpv.
    pub fn id(&self) -> u32 {
        self.window
    }

    /// Places the window (in physical pixels, relative to the main window) and shows it.
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
        // Empties the queue of X11 events/errors that we do not use, so that it
        // does not grow indefinitely.
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
