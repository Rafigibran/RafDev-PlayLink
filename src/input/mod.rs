//! Input controller (PRD section 4.3).
//!
//! Sends scrcpy control messages on the control socket opened by the mirror
//! engine. Message format (all integers big-endian):
//!
//! - Touch (0x00): action u8 (0 down, 1 up, 2 move), pointer_id u64,
//!   x u32, y u32, width u16, height u16, pressure u16, action_button u32,
//!   buttons u32. Coordinates are device screen pixels.
//! - Keycode (0x01): action u8 (0 down, 1 up), keycode u32, repeat u32,
//!   metastate u32.
//! - Scroll (0x02): x u32, y u32, width u16, height u16, hscroll i32,
//!   vscroll i32, buttons u32.
//! - Set clipboard (0x05): length u32, UTF-8 bytes (paste = 1, copy = 0
//!   flag byte first in scrcpy >= 2.0: sequence u64, paste u8, length u32).
//! - Inject text (0x06): length u32, UTF-8 bytes.

use crate::core::{PlayLinkError, Result};
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

pub const KEYCODE_BACK: u32 = 4;
pub const KEYCODE_HOME: u32 = 3;
pub const KEYCODE_APP_SWITCH: u32 = 187;
pub const KEYCODE_VOLUME_UP: u32 = 24;
pub const KEYCODE_VOLUME_DOWN: u32 = 25;
pub const KEYCODE_POWER: u32 = 26;

/// Sends control messages to the device. Owns the control socket.
pub struct InputController {
    stream: TcpStream,
    screen_width: u16,
    screen_height: u16,
}

impl InputController {
    pub fn new(stream: TcpStream, screen_width: u16, screen_height: u16) -> Self {
        Self {
            stream,
            screen_width,
            screen_height,
        }
    }

    /// Tap at device pixel coordinates.
    pub fn tap(&mut self, x: u32, y: u32) -> Result<()> {
        self.touch(0, 0, x, y, 0xFFFF, 1, 1)?;
        self.touch(1, 0, x, y, 0, 1, 0)
    }

    /// Drag from (x1, y1) to (x2, y2) in device pixels.
    pub fn swipe(&mut self, x1: u32, y1: u32, x2: u32, y2: u32) -> Result<()> {
        self.touch(0, 0, x1, y1, 0xFFFF, 1, 1)?;
        for step in 1..=5 {
            let t = step as f32 / 6.0;
            let x = x1 as f32 + (x2 as f32 - x1 as f32) * t;
            let y = y1 as f32 + (y2 as f32 - y1 as f32) * t;
            self.touch(2, 0, x as u32, y as u32, 0xFFFF, 0, 1)?;
            std::thread::sleep(Duration::from_millis(8));
        }
        self.touch(1, 0, x2, y2, 0, 1, 0)
    }

    /// Mouse wheel scroll at device pixel coordinates.
    pub fn scroll(&mut self, x: u32, y: u32, hscroll: i32, vscroll: i32) -> Result<()> {
        let mut buf = Vec::with_capacity(25);
        buf.push(0x02);
        buf.extend_from_slice(&x.to_be_bytes());
        buf.extend_from_slice(&y.to_be_bytes());
        buf.extend_from_slice(&self.screen_width.to_be_bytes());
        buf.extend_from_slice(&self.screen_height.to_be_bytes());
        buf.extend_from_slice(&hscroll.to_be_bytes());
        buf.extend_from_slice(&vscroll.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes()); // no buttons held
        self.write(&buf)
    }

    /// Press and release one Android keycode.
    pub fn key(&mut self, keycode: u32) -> Result<()> {
        self.key_event(0, keycode)?;
        self.key_event(1, keycode)
    }

    pub fn back(&mut self) -> Result<()> {
        self.key(KEYCODE_BACK)
    }

    pub fn home(&mut self) -> Result<()> {
        self.key(KEYCODE_HOME)
    }

    pub fn recent_apps(&mut self) -> Result<()> {
        self.key(KEYCODE_APP_SWITCH)
    }

    /// Type text into the focused field on the phone.
    pub fn inject_text(&mut self, text: &str) -> Result<()> {
        let bytes = text.as_bytes();
        let mut buf = Vec::with_capacity(5 + bytes.len());
        buf.push(0x06);
        buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(bytes);
        self.write(&buf)
    }

    /// Set the device clipboard (and paste when `paste` is true).
    pub fn set_clipboard(&mut self, text: &str, paste: bool) -> Result<()> {
        let bytes = text.as_bytes();
        let mut buf = Vec::with_capacity(14 + bytes.len());
        buf.push(0x05);
        buf.extend_from_slice(&0u64.to_be_bytes()); // sequence
        buf.push(if paste { 1 } else { 0 });
        buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(bytes);
        self.write(&buf)
    }

    /// One touch event (0 down, 1 up, 2 move) for the given pointer.
    fn touch(
        &mut self,
        action: u8,
        pointer_id: u64,
        x: u32,
        y: u32,
        pressure: u16,
        action_button: u32,
        buttons: u32,
    ) -> Result<()> {
        let mut buf = Vec::with_capacity(32);
        buf.push(0x00);
        buf.push(action);
        buf.extend_from_slice(&pointer_id.to_be_bytes());
        buf.extend_from_slice(&x.to_be_bytes());
        buf.extend_from_slice(&y.to_be_bytes());
        buf.extend_from_slice(&self.screen_width.to_be_bytes());
        buf.extend_from_slice(&self.screen_height.to_be_bytes());
        buf.extend_from_slice(&pressure.to_be_bytes());
        buf.extend_from_slice(&action_button.to_be_bytes());
        buf.extend_from_slice(&buttons.to_be_bytes());
        self.write(&buf)
    }

    /// One key event (0 down, 1 up).
    fn key_event(&mut self, action: u8, keycode: u32) -> Result<()> {
        let mut buf = Vec::with_capacity(14);
        buf.push(0x01);
        buf.push(action);
        buf.extend_from_slice(&keycode.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes()); // repeat
        buf.extend_from_slice(&0u32.to_be_bytes()); // metastate
        self.write(&buf)
    }

    /// Write one control message to the socket.
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.stream.write_all(buf).map_err(PlayLinkError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    /// Loopback pair: controller on the client end, reader on the server end.
    fn pair() -> (InputController, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (InputController::new(client, 1080, 2400), server)
    }

    #[test]
    fn tap_sends_down_then_up() {
        let (mut ctrl, mut server) = pair();
        ctrl.tap(100, 200).unwrap();
        let mut buf = [0u8; 64];
        server.read_exact(&mut buf).unwrap();
        // Down event (32 bytes).
        assert_eq!(buf[0], 0x00);
        assert_eq!(buf[1], 0);
        assert_eq!(u64::from_be_bytes(buf[2..10].try_into().unwrap()), 0);
        assert_eq!(u32::from_be_bytes(buf[10..14].try_into().unwrap()), 100);
        assert_eq!(u32::from_be_bytes(buf[14..18].try_into().unwrap()), 200);
        assert_eq!(u16::from_be_bytes(buf[18..20].try_into().unwrap()), 1080);
        assert_eq!(u16::from_be_bytes(buf[20..22].try_into().unwrap()), 2400);
        assert_eq!(u16::from_be_bytes(buf[22..24].try_into().unwrap()), 0xFFFF);
        assert_eq!(u32::from_be_bytes(buf[24..28].try_into().unwrap()), 1);
        assert_eq!(u32::from_be_bytes(buf[28..32].try_into().unwrap()), 1);
        // Up event.
        assert_eq!(buf[32], 0x00);
        assert_eq!(buf[33], 1);
        assert_eq!(u16::from_be_bytes(buf[54..56].try_into().unwrap()), 0);
        assert_eq!(u32::from_be_bytes(buf[60..64].try_into().unwrap()), 0);
    }

    #[test]
    fn swipe_sends_down_moves_then_up() {
        let (mut ctrl, mut server) = pair();
        ctrl.swipe(0, 0, 60, 120).unwrap();
        let mut buf = [0u8; 224];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 0x00);
        assert_eq!(buf[1], 0); // down
        for i in 0..5 {
            let off = 32 + i * 32;
            assert_eq!(buf[off], 0x00);
            assert_eq!(buf[off + 1], 2); // move
        }
        assert_eq!(buf[192], 0x00);
        assert_eq!(buf[193], 1); // up
        assert_eq!(u32::from_be_bytes(buf[202..206].try_into().unwrap()), 60);
        assert_eq!(u32::from_be_bytes(buf[206..210].try_into().unwrap()), 120);
    }

    #[test]
    fn key_sends_down_then_up() {
        let (mut ctrl, mut server) = pair();
        ctrl.back().unwrap();
        let mut buf = [0u8; 28];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 0x01);
        assert_eq!(buf[1], 0);
        assert_eq!(
            u32::from_be_bytes(buf[2..6].try_into().unwrap()),
            KEYCODE_BACK
        );
        assert_eq!(buf[14], 0x01);
        assert_eq!(buf[15], 1);
        assert_eq!(
            u32::from_be_bytes(buf[16..20].try_into().unwrap()),
            KEYCODE_BACK
        );
    }

    #[test]
    fn set_clipboard_sends_sequence_paste_length_and_text() {
        let (mut ctrl, mut server) = pair();
        ctrl.set_clipboard("hi", true).unwrap();
        let mut buf = [0u8; 16];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 0x05);
        assert_eq!(u64::from_be_bytes(buf[1..9].try_into().unwrap()), 0);
        assert_eq!(buf[9], 1);
        assert_eq!(u32::from_be_bytes(buf[10..14].try_into().unwrap()), 2);
        assert_eq!(&buf[14..16], b"hi");
    }

    #[test]
    fn inject_text_sends_length_and_text() {
        let (mut ctrl, mut server) = pair();
        ctrl.inject_text("ab").unwrap();
        let mut buf = [0u8; 7];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 0x06);
        assert_eq!(u32::from_be_bytes(buf[1..5].try_into().unwrap()), 2);
        assert_eq!(&buf[5..7], b"ab");
    }
}
