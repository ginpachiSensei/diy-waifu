import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export default function BodiesTab() {
  const [serverStatus, setServerStatus] = useState("starting");
  const [clients, setClients] = useState<string[]>([]);
  const [phoneUrl, setPhoneUrl] = useState<string | null>(null);
  const [phoneError, setPhoneError] = useState<string | null>(null);

  useEffect(() => {
    invoke<string>("get_phone_url")
      .then(setPhoneUrl)
      .catch((e) => setPhoneError(String(e)));

    const unlisten = [
      listen<string>("ws:server_status", (e) => setServerStatus(e.payload)),
      listen<string>("ws:client_connected", (e) => setClients((c) => [...c, e.payload])),
      listen<string>("ws:client_disconnected", (e) => setClients((c) => c.filter((x) => x !== e.payload))),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, []);

  return (
    <div className="stack">
      <section>
        <h3>Phone</h3>
        <p>Open this on a phone on the same Wi-Fi:</p>
        {phoneUrl ? <code className="big">{phoneUrl}</code> : <p className="error">{phoneError ?? "…"}</p>}
        <p className="hint">Voice input needs localhost (adb reverse). See phone/README.md.</p>
      </section>

      <section>
        <h3>ESP32</h3>
        <p>
          Connects to <code>ws://&lt;this PC&gt;:8080</code>. The Wokwi simulator uses{" "}
          <code>ws://host.wokwi.internal:8080</code>.
        </p>
      </section>

      <section>
        <h3>Connections</h3>
        <p>
          Server:{" "}
          {serverStatus === "connected" ? <span className="ok">running</span> : <span className="muted">starting…</span>}
          {" · "}
          {clients.length} connected
        </p>
        <p className="hint">Counts bodies that connected while this window was open. Every body gets every reply.</p>
        {clients.length > 0 && (
          <ul className="list">
            {clients.map((c) => (
              <li key={c}>
                <code>{c}</code>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
