// Viewer side of a remote session: signaling over WebSocket, media + input over WebRTC.
// The agent creates the offer (it owns the media tracks); the viewer answers.

import {
  INPUT_CHANNEL,
  PROTOCOL_VERSION,
  type AgentInfo,
  type InputEvent,
  type ServerToViewer,
  type SignalPayload,
  type ViewerToServer,
} from "./proto";

export type SessionState =
  | { phase: "connecting" }
  | { phase: "awaiting_consent" }
  | { phase: "negotiating"; info: AgentInfo }
  | { phase: "connected"; info: AgentInfo; route: string }
  | { phase: "ended"; message: string; isError: boolean };

const END_MESSAGES: Record<string, string> = {
  rejected: "The remote user declined the session.",
  agent_disconnected: "The remote device went offline.",
  agent_ended: "The remote device ended the session.",
  viewer_ended: "Session ended.",
};

export class Session {
  private ws: WebSocket;
  private pc: RTCPeerConnection | null = null;
  private input: RTCDataChannel | null = null;
  private pendingIce: RTCIceCandidateInit[] = [];
  private finished = false;

  constructor(
    deviceId: string,
    password: string,
    private onState: (s: SessionState) => void,
    private onStream: (s: MediaStream) => void,
  ) {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    this.ws = new WebSocket(`${proto}//${location.host}/ws/viewer`);
    this.onState({ phase: "connecting" });
    this.ws.onopen = () =>
      this.send({
        type: "connect",
        protocol_version: PROTOCOL_VERSION,
        device_id: deviceId.replace(/\s+/g, ""),
        password,
      });
    this.ws.onmessage = (e) => this.handle(JSON.parse(e.data) as ServerToViewer);
    this.ws.onclose = () => this.finish("Connection to server closed.", true);
    this.ws.onerror = () => this.finish("Could not reach the server.", true);
  }

  sendInput(ev: InputEvent) {
    if (this.input?.readyState === "open") this.input.send(JSON.stringify(ev));
  }

  close() {
    if (this.ws.readyState === WebSocket.OPEN) this.send({ type: "end_session" });
    this.finish(END_MESSAGES.viewer_ended, false);
  }

  private send(m: ViewerToServer) {
    if (this.ws.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(m));
  }

  private async handle(m: ServerToViewer) {
    switch (m.type) {
      case "awaiting_consent":
        this.onState({ phase: "awaiting_consent" });
        break;
      case "session_started":
        this.startPeer(m.ice_servers, m.info);
        break;
      case "signal":
        await this.onSignal(m.payload);
        break;
      case "session_ended":
        this.finish(END_MESSAGES[m.reason] ?? "Session ended.", m.reason !== "viewer_ended");
        break;
      case "error":
        this.finish(m.message, true);
        break;
    }
  }

  private startPeer(iceServers: RTCIceServer[], info: AgentInfo) {
    const pc = new RTCPeerConnection({ iceServers });
    this.pc = pc;
    this.onState({ phase: "negotiating", info });
    pc.onicecandidate = (e) => {
      if (!e.candidate) return;
      this.send({
        type: "signal",
        payload: {
          kind: "ice",
          candidate: e.candidate.candidate,
          sdp_mid: e.candidate.sdpMid ?? undefined,
          sdp_mline_index: e.candidate.sdpMLineIndex ?? undefined,
        },
      });
    };
    pc.ontrack = (e) => this.onStream(e.streams[0] ?? new MediaStream([e.track]));
    pc.ondatachannel = (e) => {
      if (e.channel.label === INPUT_CHANNEL) this.input = e.channel;
    };
    pc.onconnectionstatechange = async () => {
      if (pc.connectionState === "connected") {
        this.onState({ phase: "connected", info, route: await describeRoute(pc) });
      } else if (pc.connectionState === "failed") {
        this.finish("Peer connection failed (no direct route and no relay).", true);
      }
    };
  }

  private async onSignal(p: SignalPayload) {
    const pc = this.pc;
    if (!pc) return;
    if (p.kind === "offer") {
      await pc.setRemoteDescription({ type: "offer", sdp: p.sdp });
      for (const c of this.pendingIce.splice(0)) await pc.addIceCandidate(c);
      const answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);
      this.send({ type: "signal", payload: { kind: "answer", sdp: answer.sdp ?? "" } });
    } else if (p.kind === "ice") {
      const c = { candidate: p.candidate, sdpMid: p.sdp_mid, sdpMLineIndex: p.sdp_mline_index };
      if (pc.remoteDescription) await pc.addIceCandidate(c);
      else this.pendingIce.push(c);
    }
  }

  private finish(message: string, isError: boolean) {
    if (this.finished) return;
    this.finished = true;
    this.pc?.close();
    this.ws.close();
    this.onState({ phase: "ended", message, isError });
  }
}

/** "Direct" when the selected ICE pair avoids TURN, else "Relayed". */
async function describeRoute(pc: RTCPeerConnection): Promise<string> {
  const stats = await pc.getStats();
  let route = "Direct";
  stats.forEach((s) => {
    if (s.type === "candidate-pair" && s.state === "succeeded" && s.nominated) {
      const local = stats.get(s.localCandidateId);
      const remote = stats.get(s.remoteCandidateId);
      if (local?.candidateType === "relay" || remote?.candidateType === "relay") route = "Relayed";
    }
  });
  return route;
}
