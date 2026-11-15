# Remote Desk

Self-hostable, AnyDesk-style remote desktop. See [docs/PLAN.md](docs/PLAN.md) for the
architecture and roadmap.

| Path | What |
|---|---|
| `crates/proto` | Signaling + input wire types (mirrored in `web/src/proto.ts`) |
| `crates/server` | Control plane + signaling server (`rd-server`) |
| `crates/agent` | Host agent (`rd-agent`) |
| `web/` | Browser viewer (React + WebRTC) |
| `deploy/` | Dockerfile, docker-compose dev stack (server + coturn) |
| `prototype/` | Original Python proof of concept (reference only) |

## Status

Phase 1 (foundation) done: device registration with stable 9-digit IDs, password +
consent-gated sessions, WebRTC signaling relay, brute-force rate limiting, browser
viewer. **Not yet:** screen capture/encoding and input injection in the agent (Phase 2),
so a session currently connects and negotiates but shows no picture.

## Run locally

    cargo run -p remote-desk-server                      # :8080
    cargo run -p remote-desk-agent                       # prints Device ID + one-time password
    cd web && npm install && npm run dev                 # viewer at http://localhost:5173

Or serve the built viewer from the server: `npm run build` in `web/`, then
`cargo run -p remote-desk-server -- --web-dir web/dist` and open http://localhost:8080.

Agent options: `--set-password <pw>` stores an unattended-access password (Argon2id hash
only), `--unattended` skips the consent prompt, `--server wss://…` points at a remote
control plane. Identity lives in the OS config dir (`remote-desk/agent.json`, mode 0600).

## Test

    cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cd web && npm run build

## Security notes (Phase 1)

- Run behind TLS (`wss://`) for anything beyond localhost; the viewer password travels
  over the signaling socket.
- The server never stores plaintext passwords; agent-supplied hashes must be argon2id
  with bounded cost parameters (prevents a rogue agent from DoSing logins).
- Unknown device and wrong password return the same error with equalized timing.
- Rate limiting keys on the TCP peer IP; behind a reverse proxy, all viewers share the
  proxy's IP until trusted `X-Forwarded-For` handling is added.
- Device registry is in memory: a server restart lets agents re-claim their saved IDs
  first-come. Phase 3 replaces this with enrollment + device certificates.
