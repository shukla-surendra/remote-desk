//! Signaling protocol shared by the agent, the control plane and the web viewer.
//!
//! All messages are JSON text frames over WebSocket, tagged by `type` (snake_case).
//! The TypeScript mirror lives in `web/src/proto.ts`; keep the two in sync.
//!
//! Flow:
//! 1. Agent connects to `/ws/agent`, sends [`AgentToServer::Register`], gets
//!    [`ServerToAgent::Registered`] with its device ID (and a token to reclaim it).
//! 2. Viewer connects to `/ws/viewer`, sends [`ViewerToServer::Connect`].
//! 3. Server verifies the access password, then sends [`ServerToAgent::SessionRequest`].
//! 4. Agent answers with [`AgentToServer::SessionDecision`] (consent prompt or policy).
//! 5. On accept, the viewer receives [`ServerToViewer::SessionStarted`] and both sides
//!    exchange [`SignalPayload`]s (agent sends the offer, viewer answers) until a
//!    WebRTC peer connection is up. Media and input then flow P2P (or via TURN).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Protocol version; bumped on breaking changes. Server rejects mismatched majors.
pub const PROTOCOL_VERSION: u32 = 1;

/// Data channel label used for input events (viewer -> agent).
pub const INPUT_CHANNEL: &str = "input";

pub type SessionId = Uuid;

/// WebRTC negotiation messages, relayed opaquely by the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalPayload {
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    Ice {
        candidate: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdp_mid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdp_mline_index: Option<u16>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub hostname: String,
    pub os: String,
    pub agent_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentToServer {
    Register {
        protocol_version: u32,
        /// Previously assigned ID + reclaim token; omitted on first run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_token: Option<String>,
        /// Argon2 PHC string of the unattended-access password. The plaintext never
        /// leaves the agent at registration time.
        password_hash: String,
        info: AgentInfo,
    },
    SessionDecision {
        session_id: SessionId,
        accept: bool,
    },
    Signal {
        session_id: SessionId,
        payload: SignalPayload,
    },
    EndSession {
        session_id: SessionId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerToAgent {
    Registered {
        device_id: String,
        device_token: String,
        ice_servers: Vec<IceServer>,
    },
    SessionRequest {
        session_id: SessionId,
        viewer_addr: String,
    },
    Signal {
        session_id: SessionId,
        payload: SignalPayload,
    },
    SessionEnded {
        session_id: SessionId,
    },
    Error(ErrorBody),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewerToServer {
    Connect {
        protocol_version: u32,
        device_id: String,
        password: String,
    },
    Signal {
        payload: SignalPayload,
    },
    EndSession,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerToViewer {
    /// Credentials accepted; waiting for the agent's consent decision.
    AwaitingConsent {
        session_id: SessionId,
    },
    SessionStarted {
        session_id: SessionId,
        ice_servers: Vec<IceServer>,
        info: AgentInfo,
    },
    Signal {
        payload: SignalPayload,
    },
    SessionEnded {
        reason: EndReason,
    },
    Error(ErrorBody),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Rejected,
    AgentDisconnected,
    AgentEnded,
    ViewerEnded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    UnsupportedVersion,
    /// Unknown device or wrong password; deliberately indistinguishable.
    InvalidCredentials,
    RateLimited,
    DeviceBusy,
    DeviceIdTaken,
    Internal,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// Input events sent over the [`INPUT_CHANNEL`] data channel.
/// Coordinates are normalized to `0.0..=1.0` of the shared display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove {
        x: f32,
        y: f32,
    },
    MouseDown {
        x: f32,
        y: f32,
        button: MouseButton,
    },
    MouseUp {
        x: f32,
        y: f32,
        button: MouseButton,
    },
    Wheel {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    /// `code` is the DOM `KeyboardEvent.code` (layout-independent physical key).
    KeyDown {
        code: String,
    },
    KeyUp {
        code: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn signal_payload_wire_format() {
        let m = ViewerToServer::Signal {
            payload: SignalPayload::Ice {
                candidate: "candidate:1".into(),
                sdp_mid: Some("0".into()),
                sdp_mline_index: None,
            },
        };
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            json!({"type":"signal","payload":{"kind":"ice","candidate":"candidate:1","sdp_mid":"0"}})
        );
    }

    #[test]
    fn error_is_flattened() {
        let m = ServerToViewer::Error(ErrorBody::new(ErrorCode::RateLimited, "slow down"));
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            json!({"type":"error","code":"rate_limited","message":"slow down"})
        );
    }

    #[test]
    fn input_event_roundtrip() {
        let raw = r#"{"type":"mouse_down","x":0.5,"y":0.25,"button":"right"}"#;
        let ev: InputEvent = serde_json::from_str(raw).unwrap();
        assert_eq!(
            ev,
            InputEvent::MouseDown {
                x: 0.5,
                y: 0.25,
                button: MouseButton::Right
            }
        );
    }
}
