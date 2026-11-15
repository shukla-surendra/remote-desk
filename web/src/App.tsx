import { useEffect, useRef, useState, type FormEvent } from "react";
import { Session, type SessionState } from "./session";
import { RemoteScreen } from "./RemoteScreen";

export function App() {
  const [deviceId, setDeviceId] = useState("");
  const [password, setPassword] = useState("");
  const [state, setState] = useState<SessionState | null>(null);
  const [stream, setStream] = useState<MediaStream | null>(null);
  const session = useRef<Session | null>(null);

  useEffect(() => () => session.current?.close(), []);

  const connect = (e: FormEvent) => {
    e.preventDefault();
    session.current?.close();
    setStream(null);
    session.current = new Session(deviceId, password, setState, setStream);
    setPassword("");
  };

  const disconnect = () => session.current?.close();
  const active = state && state.phase !== "ended";

  if (active) {
    return (
      <div className="session">
        <header className="toolbar">
          <span className="status">{statusText(state)}</span>
          <button onClick={disconnect}>Disconnect</button>
        </header>
        {state.phase === "negotiating" || state.phase === "connected" ? (
          <RemoteScreen stream={stream} onInput={(ev) => session.current?.sendInput(ev)} />
        ) : (
          <div className="waiting">{statusText(state)}</div>
        )}
      </div>
    );
  }

  return (
    <main className="login">
      <h1>Remote Desk</h1>
      <form onSubmit={connect}>
        <label>
          Remote device ID
          <input
            value={deviceId}
            onChange={(e) => setDeviceId(e.target.value)}
            placeholder="123 456 789"
            inputMode="numeric"
            autoComplete="off"
            required
          />
        </label>
        <label>
          Password
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="current-password"
            required
          />
        </label>
        <button type="submit">Connect</button>
      </form>
      {state?.phase === "ended" && (
        <p className={state.isError ? "error" : "note"} role="status">
          {state.message}
        </p>
      )}
    </main>
  );
}

function statusText(s: SessionState): string {
  switch (s.phase) {
    case "connecting":
      return "Connecting…";
    case "awaiting_consent":
      return "Waiting for the remote user to accept…";
    case "negotiating":
      return `Connecting to ${s.info.hostname}…`;
    case "connected":
      return `${s.info.hostname} · ${s.route}`;
    case "ended":
      return s.message;
  }
}
