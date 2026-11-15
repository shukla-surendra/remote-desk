// Mirror of crates/proto/src/lib.rs. Keep in sync.

export const PROTOCOL_VERSION = 1;
export const INPUT_CHANNEL = "input";

export type SignalPayload =
  | { kind: "offer"; sdp: string }
  | { kind: "answer"; sdp: string }
  | { kind: "ice"; candidate: string; sdp_mid?: string; sdp_mline_index?: number };

export interface AgentInfo {
  hostname: string;
  os: string;
  agent_version: string;
}

export interface IceServer {
  urls: string[];
  username?: string;
  credential?: string;
}

export type ErrorCode =
  | "bad_request"
  | "unsupported_version"
  | "invalid_credentials"
  | "rate_limited"
  | "device_busy"
  | "device_id_taken"
  | "internal";

export type EndReason = "rejected" | "agent_disconnected" | "agent_ended" | "viewer_ended";

export type ViewerToServer =
  | { type: "connect"; protocol_version: number; device_id: string; password: string }
  | { type: "signal"; payload: SignalPayload }
  | { type: "end_session" };

export type ServerToViewer =
  | { type: "awaiting_consent"; session_id: string }
  | { type: "session_started"; session_id: string; ice_servers: IceServer[]; info: AgentInfo }
  | { type: "signal"; payload: SignalPayload }
  | { type: "session_ended"; reason: EndReason }
  | { type: "error"; code: ErrorCode; message: string };

export type MouseButton = "left" | "middle" | "right";

export type InputEvent =
  | { type: "mouse_move"; x: number; y: number }
  | { type: "mouse_down"; x: number; y: number; button: MouseButton }
  | { type: "mouse_up"; x: number; y: number; button: MouseButton }
  | { type: "wheel"; x: number; y: number; dx: number; dy: number }
  | { type: "key_down"; code: string }
  | { type: "key_up"; code: string };
