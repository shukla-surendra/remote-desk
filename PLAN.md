# Remote Desk Plan

Goal: remotely connect my own laptops (2 Macs + 1 Windows PC) with an AnyDesk-style
experience, using RustDesk. Start peer-to-peer on default servers; add a self-hosted
relay later only if needed.

> Note: with RustDesk defaults, screen traffic goes peer-to-peer when possible. The
> public ID server only helps machines find each other, and a public relay is used as a
> fallback if a direct connection fails.

## Background: how a connection works

1. **ID/rendezvous server**: both machines register with it; it tells your machine where
   the other one is.
2. **Direct (P2P) connection**: machines try to connect straight to each other. Fastest;
   no relay.
3. **Relay fallback**: if NAT/firewalls block a direct connection, traffic goes through a
   relay server.

Direct connections commonly fail on mobile hotspots, office/university networks and some
ISPs.

## Phase 1: P2P with RustDesk defaults (now)

1. Install RustDesk on all 3 machines (rustdesk.com).
2. Set a permanent password on each (Settings -> Security) for unattended access.
3. **Mac only:** grant Screen Recording, Accessibility and Input Monitoring in
   System Settings -> Privacy & Security. Enable "Start on boot".
4. **Windows:** install RustDesk as a service (it prompts) so it works at the login screen.
5. Disable sleep when plugged in (Mac: "Prevent automatic sleeping on power adapter";
   Windows: Sleep = Never when plugged in).
6. Record each machine's ID in the RustDesk address book.
7. **Verify direct connections:** connect between every pair, in both directions, from
   home and from an outside network (e.g. phone hotspot). Note whether the session shows
   "Direct" or "Relayed" and record the result below.

### Test log

| From -> To | Network | Direct / Relayed | Quality notes |
|---|---|---|---|
| | | | |

## Phase 2: Decide if a relay is needed (after ~1-2 weeks)

Move to Phase 3 if any of these happen:

- Sessions are laggy or low quality.
- Sessions are often "Relayed" rather than "Direct".
- Connections fail from certain networks (office, hotel, mobile).
- I want traffic to avoid public servers.

If none apply, stay on Phase 1.

## Phase 3: Self-hosted relay (later)

1. Rent a small VPS (1 vCPU / 1 GB RAM is enough) in a region close to me.
2. Run RustDesk `hbbs` (ID server) and `hbbr` (relay) with Docker.
3. Open ports: TCP 21115-21119 and UDP 21116.
4. Copy the server's public key.
5. On each machine: Settings -> Network -> ID/Relay server; enter the server address
   and key.
6. Re-test any pairs that were relayed in Phase 1.

Nothing from Phase 1 is discarded: only the server address is added in settings;
passwords, permissions and address book stay as they are.

## Optional fallback: Tailscale

If a pair can't connect directly, install Tailscale (free for personal use) on all three
machines and use RustDesk's "direct IP access" with the Tailscale addresses. May remove
the need for a self-hosted relay.

## Alternatives considered

- **Keep AnyDesk**: free for personal use, but relay servers can't be self-hosted.
- **Built-in tools (Screen Sharing / RDP) + Tailscale**: better performance, but two
  different workflows; Windows Home has no RDP host.
- **Chrome Remote Desktop**: easiest, fewer features.
- **Parsec**: best low-latency performance.
