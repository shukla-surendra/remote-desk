import { useEffect, useRef } from "react";
import type { InputEvent, MouseButton } from "./proto";

const BUTTONS: Record<number, MouseButton> = { 0: "left", 1: "middle", 2: "right" };
const MOVE_INTERVAL_MS = 16;

interface Props {
  stream: MediaStream | null;
  onInput: (ev: InputEvent) => void;
}

/** Video surface that maps pointer/keyboard events to normalized remote coordinates. */
export function RemoteScreen({ stream, onInput }: Props) {
  const video = useRef<HTMLVideoElement>(null);
  const lastMove = useRef(0);

  useEffect(() => {
    if (video.current) video.current.srcObject = stream;
  }, [stream]);

  // Account for letterboxing from object-fit: contain.
  const toRemote = (clientX: number, clientY: number) => {
    const v = video.current!;
    const r = v.getBoundingClientRect();
    const vw = v.videoWidth || r.width;
    const vh = v.videoHeight || r.height;
    const scale = Math.min(r.width / vw, r.height / vh);
    const w = vw * scale;
    const h = vh * scale;
    const x = (clientX - r.left - (r.width - w) / 2) / w;
    const y = (clientY - r.top - (r.height - h) / 2) / h;
    return { x: clamp(x), y: clamp(y) };
  };

  return (
    <video
      ref={video}
      className="screen"
      autoPlay
      playsInline
      muted
      tabIndex={0}
      onMouseMove={(e) => {
        const now = performance.now();
        if (now - lastMove.current < MOVE_INTERVAL_MS) return;
        lastMove.current = now;
        onInput({ type: "mouse_move", ...toRemote(e.clientX, e.clientY) });
      }}
      onMouseDown={(e) => {
        e.currentTarget.focus();
        onInput({ type: "mouse_down", button: BUTTONS[e.button] ?? "left", ...toRemote(e.clientX, e.clientY) });
      }}
      onMouseUp={(e) =>
        onInput({ type: "mouse_up", button: BUTTONS[e.button] ?? "left", ...toRemote(e.clientX, e.clientY) })
      }
      onWheel={(e) => onInput({ type: "wheel", dx: e.deltaX, dy: e.deltaY, ...toRemote(e.clientX, e.clientY) })}
      onContextMenu={(e) => e.preventDefault()}
      onKeyDown={(e) => {
        e.preventDefault();
        onInput({ type: "key_down", code: e.code }); // repeats pass through as OS auto-repeat would
      }}
      onKeyUp={(e) => {
        e.preventDefault();
        onInput({ type: "key_up", code: e.code });
      }}
    />
  );
}

const clamp = (n: number) => Math.min(1, Math.max(0, n));
