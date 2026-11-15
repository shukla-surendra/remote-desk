# Remote Desk — Enterprise Plan

## 1. Language decision
| Component | Language | Why |
|---|---|---|
| Host agent (capture, input, encode) | **Rust** | Native perf, no GC pauses, small single binary, safe FFI to OS APIs (CoreGraphics/DXGI/X11-Wayland), runs as a system service |
| Control plane (auth, directory, policy, audit API) | **Rust (axum)** | One language/toolchain, shared protocol crate with the agent |
| Signaling + relay (TURN-like) | **Rust (tokio)** | High-concurrency async I/O; relay must be cheap per session |
| Web viewer + admin console | **TypeScript (React)** | Browser is the zero-install client; WebRTC native there |
| Data | PostgreSQL (state, audit), Redis (presence/signaling fan-out) | Standard, HA-friendly |

Python prototype stays as `prototype/` for reference only.

## 2. Architecture
    Viewer (browser/native) ──WebRTC (DTLS-SRTP, P2P)──► Agent
            │  signaling (WSS, JWT)                         │ mTLS
            └──────────► Control plane ◄────────────────────┘
                          │  Postgres / Redis / Audit sink
                          └─ Relay (TURN) when P2P fails

- **Media**: WebRTC with H.264/VP9 hardware encode (VideoToolbox / NVENC / VAAPI), input + clipboard + file transfer over data channels. Replaces JPEG-over-WebSocket.
- **Identity**: OIDC/SAML SSO (Okta, Entra, Google), SCIM provisioning, MFA enforced by IdP.
- **Authorization**: RBAC + per-device policy (who can connect, view-only vs control, time windows, require-consent).
- **Device trust**: agent enrolls with one-time token → receives device cert; mTLS to control plane; unattended access gated by policy.
- **Privacy/consent**: on-screen indicator, user prompt for attended sessions, privacy screen for unattended.
- **Audit**: immutable log of every session (who, what device, start/stop, file transfers); export to SIEM (syslog/OTLP/S3); optional session recording.
- **Multi-tenancy**: org → groups → devices, tenant-scoped everything.
- **Ops**: stateless control plane on Kubernetes, Helm chart, OpenTelemetry metrics/traces, health probes, signed releases + SBOM, auto-update for agent.

## 3. Repo layout (Cargo workspace)
    crates/proto      shared wire types / signaling messages
    crates/agent      host agent (service/daemon)
    crates/server     control plane + signaling (axum)
    crates/relay      TURN/relay
    web/              React viewer + admin console
    deploy/           Dockerfiles, Helm, docker-compose (dev)
    docs/             PLAN, ADRs, threat model
    prototype/        current Python PoC

## 4. Phases
1. **Foundation** (wk 1–2): workspace, CI (fmt/clippy/test/audit), proto crate, move PoC to `prototype/`, docker-compose dev stack.
2. **Core streaming** (wk 3–5): Rust agent capture + H.264 encode + input injection; WebRTC session via signaling; browser viewer; TURN fallback.
3. **Identity & policy** (wk 6–8): OIDC login, orgs/RBAC, device enrollment + mTLS, consent flow.
4. **Enterprise features** (wk 9–12): audit log + SIEM export, unattended access, clipboard/file transfer, multi-monitor, session recording.
5. **Hardening & ops** (wk 13–14): threat model + pen test, load tests, Helm/K8s, observability, auto-update, signed installers (.pkg/.msi/.deb).

## 5. Security baseline
TLS 1.3 everywhere, E2E-encrypted media (DTLS-SRTP), no passwords in URLs/logs, short-lived JWTs, rate limiting, dependency audit (`cargo audit/deny`), least-privilege service accounts, secrets via env/K8s secrets/Vault.

## 6. Progress
- [x] **Phase 1 – Foundation**: Cargo workspace, `proto` crate (+ TS mirror), signaling server (in-memory registry, argon2id password gate, consent handshake, rate limiting), agent (stable ID, unattended/one-time password, consent prompt, reconnect), React viewer (WebRTC answerer, input mapping), CI, Dockerfile + compose (server + coturn), integration tests.
- [ ] **Phase 2 – Core streaming**: next up — macOS capture (ScreenCaptureKit) → VideoToolbox H.264 → `webrtc-rs` peer in the agent; input injection (CGEvent) from the `input` data channel.
- Deferred from Phase 1: `crates/relay` (coturn covers TURN for now), Postgres/Redis in compose (unused until Phase 3).

Working defaults for the open decisions below (revisit anytime): self-hosted first, macOS agent first, browser-only viewer, no compliance target yet.

## 7. Open decisions
- Self-hosted only, or SaaS multi-tenant too?
- Target OSes for agent first (macOS only → Windows → Linux)?
- Native viewer app needed, or browser-only?
- Compliance targets (SOC2, HIPAA, ISO 27001) — drives audit/recording scope.
