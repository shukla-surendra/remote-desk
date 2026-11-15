"""Relay server: pairs a host (shared screen) with a browser viewer by ID + password."""
import argparse, asyncio, hmac, secrets, time
from pathlib import Path
from aiohttp import web, WSMsgType

hosts = {}        # id -> {"ws", "password", "viewer"}
fails = {}        # ip -> [timestamps]
MAX_FAILS, WINDOW = 5, 300

def rate_limited(ip):
    now = time.time()
    fails[ip] = [t for t in fails.get(ip, []) if now - t < WINDOW]
    return len(fails[ip]) >= MAX_FAILS

async def host_ws(request):
    ws = web.WebSocketResponse(max_msg_size=0)
    await ws.prepare(request)
    msg = await ws.receive_json()
    if msg.get("type") != "register" or len(msg.get("password", "")) < 6:
        await ws.close(); return ws
    hid = next(i for i in iter(lambda: str(secrets.randbelow(10**9)).zfill(9), None) if i not in hosts)
    h = {"ws": ws, "password": msg["password"], "viewer": None}
    hosts[hid] = h
    await ws.send_json({"type": "registered", "id": hid})
    try:
        async for m in ws:
            v = h["viewer"]
            if v and not v.closed:
                if m.type == WSMsgType.BINARY: await v.send_bytes(m.data)
                elif m.type == WSMsgType.TEXT: await v.send_str(m.data)
    finally:
        hosts.pop(hid, None)
        if h["viewer"] and not h["viewer"].closed: await h["viewer"].close()
    return ws

async def viewer_ws(request):
    ws = web.WebSocketResponse(max_msg_size=1 << 20)
    await ws.prepare(request)
    ip = request.remote
    if rate_limited(ip):
        await ws.send_json({"type": "error", "error": "Too many attempts, try later"}); await ws.close(); return ws
    msg = await ws.receive_json()
    h = hosts.get(str(msg.get("id", "")))
    if not h or not hmac.compare_digest(h["password"], str(msg.get("password", ""))):
        fails.setdefault(ip, []).append(time.time())
        await ws.send_json({"type": "error", "error": "Invalid ID or password"}); await ws.close(); return ws
    if h["viewer"] and not h["viewer"].closed:
        await ws.send_json({"type": "error", "error": "Host already has a viewer"}); await ws.close(); return ws
    h["viewer"] = ws
    await ws.send_json({"type": "connected"})
    await h["ws"].send_json({"type": "viewer", "connected": True, "ip": ip})
    try:
        async for m in ws:
            if m.type == WSMsgType.TEXT and not h["ws"].closed:
                await h["ws"].send_str(m.data)
    finally:
        h["viewer"] = None
        if not h["ws"].closed: await h["ws"].send_json({"type": "viewer", "connected": False})
    return ws

async def index(_): return web.FileResponse(Path(__file__).parent / "static" / "viewer.html")

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--host", default="0.0.0.0"); p.add_argument("--port", type=int, default=8765)
    a = p.parse_args()
    app = web.Application()
    app.add_routes([web.get("/", index), web.get("/ws/host", host_ws), web.get("/ws/viewer", viewer_ws)])
    web.run_app(app, host=a.host, port=a.port)

if __name__ == "__main__": main()
