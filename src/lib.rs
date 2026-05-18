//! Shared types and helpers for the remote-lab experiment.

pub mod session;

use serde::{Deserialize, Serialize};

/// First byte of a binary frame from host: JPEG payload follows after 8-byte LE header (w, h).
pub const FRAME_MAGIC: u8 = 0xF1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputMessage {
    MouseMove { x: f32, y: f32 },
    MouseDown { button: MouseButton },
    MouseUp { button: MouseButton },
    Scroll { dx: f32, dy: f32 },
    KeyDown { key: String },
    KeyUp { key: String },
    /// Push a text snippet from the viewer into the host's clipboard.
    ClipboardWrite { text: String },
}

/// Text-frame messages the host sends to the viewer (binary frames stay JPEG).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// Sent once after WS upgrade so the viewer knows the session is live.
    Welcome { host: String, version: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

pub fn parse_frame_header(data: &[u8]) -> Option<(u32, u32, &[u8])> {
    if data.len() < 9 || data[0] != FRAME_MAGIC {
        return None;
    }
    let w = u32::from_le_bytes(data[1..5].try_into().ok()?);
    let h = u32::from_le_bytes(data[5..9].try_into().ok()?);
    let jpeg = data.get(9..)?;
    Some((w, h, jpeg))
}
