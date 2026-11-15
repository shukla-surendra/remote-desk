"""Host agent: run on the machine you want to control. Streams screen, applies remote input."""
import argparse, asyncio, io, json, secrets, string
import aiohttp, mss, pyautogui
from PIL import Image

pyautogui.FAILSAFE = False
pyautogui.PAUSE = 0
KEYMAP = {"Enter": "enter", "Backspace": "backspace", "Tab": "tab", "Escape": "esc", " ": "space",
          "ArrowUp": "up", "ArrowDown": "down", "ArrowLeft": "left", "ArrowRight": "right",
          "Delete": "delete", "Home": "home", "End": "end", "PageUp": "pageup", "PageDown": "pagedown",
          "Control": "ctrl", "Shift": "shift", "Alt": "alt", "Meta": "win", "CapsLock": "capslock"}
BUTTONS = {0: "left", 1: "middle", 2: "right"}

def grab(sct, mon, scale, quality):
    shot = sct.grab(mon)
    img = Image.frombytes("RGB", shot.size, shot.bgra, "raw", "BGRX")
    if scale < 1: img = img.resize((int(img.width * scale), int(img.height * scale)), Image.BILINEAR)
    buf = io.BytesIO(); img.save(buf, "JPEG", quality=quality)
    return buf.getvalue()

def apply(ev, mon):
    t = ev.get("type")
    if t in ("move", "down", "up", "wheel"):
        x, y = mon["left"] + int(ev["x"] * mon["width"]), mon["top"] + int(ev["y"] * mon["height"])
        if t == "move": pyautogui.moveTo(x, y)
        elif t == "down": pyautogui.mouseDown(x, y, BUTTONS.get(ev.get("button", 0), "left"))
        elif t == "up": pyautogui.mouseUp(x, y, BUTTONS.get(ev.get("button", 0), "left"))
        else: pyautogui.scroll(-int(ev.get("dy", 0)) // 40, x, y)
    elif t in ("keydown", "keyup"):
        k = KEYMAP.get(ev["key"], ev["key"].lower() if len(ev["key"]) == 1 else ev["key"].lower())
        try: (pyautogui.keyDown if t == "keydown" else pyautogui.keyUp)(k)
        except Exception: pass

async def run(a):
    password = a.password or "".join(secrets.choice(string.ascii_letters + string.digits) for _ in range(10))
    loop = asyncio.get_running_loop()
    async with aiohttp.ClientSession() as s, s.ws_connect(a.server.rstrip("/") + "/ws/host", max_msg_size=0) as ws:
        await ws.send_json({"type": "register", "password": password})
        info = await ws.receive_json()
        print(f"\n  Your ID:       {info['id']}\n  Your password: {password}\n")
        viewing = asyncio.Event()
        with mss.mss() as sct:
            mon = sct.monitors[a.monitor]
            async def stream():
                while True:
                    await viewing.wait()
                    frame = await loop.run_in_executor(None, grab, sct, mon, a.scale, a.quality)
                    await ws.send_bytes(frame)
                    await asyncio.sleep(1 / a.fps)
            task = asyncio.create_task(stream())
            async for m in ws:
                if m.type != aiohttp.WSMsgType.TEXT: continue
                ev = json.loads(m.data)
                if ev.get("type") == "viewer":
                    if ev["connected"]:
                        if a.ask and input(f"Allow connection from {ev.get('ip')}? [y/N] ").lower() != "y":
                            continue  # viewer sees a blank screen; no control granted
                        print("Viewer connected"); viewing.set()
                    else: print("Viewer disconnected"); viewing.clear()
                elif viewing.is_set(): await loop.run_in_executor(None, apply, ev, mon)
            task.cancel()

if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--server", default="ws://localhost:8765")
    p.add_argument("--password"); p.add_argument("--fps", type=int, default=10)
    p.add_argument("--scale", type=float, default=0.75); p.add_argument("--quality", type=int, default=60)
    p.add_argument("--monitor", type=int, default=1); p.add_argument("--ask", action="store_true", help="confirm each connection in terminal")
    asyncio.run(run(p.parse_args()))
