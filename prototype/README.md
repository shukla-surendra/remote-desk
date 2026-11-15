> **Reference only.** Superseded by the Rust workspace at the repo root; see ../README.md.

# Remote Desk

Minimal self-hosted AnyDesk-style remote desktop: relay server + host agent + browser viewer.

    pip install -r requirements.txt

1. **Server** (any machine reachable by both sides): `python server.py --port 8765`
2. **Host** (machine to control): `python host.py --server ws://SERVER:8765 [--ask]` → prints ID + password
3. **Viewer**: open `http://SERVER:8765` in a browser, enter ID + password.

macOS host: grant Terminal *Screen Recording* and *Accessibility* in System Settings → Privacy & Security.

## Security
- Use TLS for anything beyond a LAN (put server behind Caddy/nginx → `wss://`; host uses `--server wss://...`). Without it, password and screen travel in clear text.
- Viewer attempts are rate-limited (5 per 5 min per IP). Use `--ask` to approve each connection.
- Only run on machines you own or are authorized to access.

## Limits
JPEG full-frame streaming (no delta/H.264), no clipboard/file transfer, one viewer per host, single monitor. Next steps: WebRTC for P2P + lower latency.
