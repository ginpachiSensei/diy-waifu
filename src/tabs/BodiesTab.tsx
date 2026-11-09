import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Pairing, errorText } from "../api";

export default function BodiesTab() {
  const [serverStatus, setServerStatus] = useState("starting");
  const [clients, setClients] = useState<string[]>([]);
  const [pairing, setPairing] = useState<Pairing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirmNew, setConfirmNew] = useState(false);

  useEffect(() => {
    invoke<Pairing>("get_pairing").then(setPairing).catch((e) => setError(errorText(e)));

    const unlisten = [
      listen<string>("ws:server_status", (e) => setServerStatus(e.payload)),
      listen<string>("ws:client_connected", (e) => setClients((c) => [...c, e.payload])),
      listen<string>("ws:client_disconnected", (e) => setClients((c) => c.filter((x) => x !== e.payload))),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, []);

  async function newCode() {
    if (!confirmNew) {
      setConfirmNew(true);
      setTimeout(() => setConfirmNew(false), 3000);
      return;
    }
    setConfirmNew(false);
    try {
      setPairing(await invoke<Pairing>("new_pairing_code"));
    } catch (e) {
      setError(errorText(e));
    }
  }

  return (
    <div className="stack">
      {error && <p className="error">{error}</p>}

      <section>
        <h3>Pairing code</h3>
        <p>
          <code className="big">{pairing?.code ?? "…"}</code>{" "}
          <button onClick={newCode}>{confirmNew ? "Sure? Others disconnect" : "New code"}</button>
        </p>
        <p className="hint">
          Phones and ESP32s on your Wi-Fi need this code to connect. The pet on this PC doesn't. A new code
          disconnects every other device until it's given the new one.
        </p>
      </section>

      <section>
        <h3>Phone</h3>
        {pairing?.phoneUrl ? (
          <>
            <p>Scan this with a phone on the same Wi-Fi, or open the link:</p>
            {pairing.qrSvg && <div className="qr" dangerouslySetInnerHTML={{ __html: pairing.qrSvg }} />}
            <code className="big">{pairing.phoneUrl}</code>
          </>
        ) : (
          <p className="error">This PC isn't on a network right now.</p>
        )}
        <p className="hint">
          The phone remembers the code. Voice input on the phone needs localhost (adb reverse). See phone/README.md.
        </p>
      </section>

      <section>
        <h3>ESP32</h3>
        <p>
          On first start it opens a Wi-Fi hotspot called <code>Companion-Setup</code>. Join it and enter your Wi-Fi,
          the pairing code, and this PC's address{" "}
          <code>{pairing?.lanIp ?? "…"}</code> (or leave it blank to find <code>diy-waifu.local</code>
          automatically).
        </p>
        <p className="hint">
          The Wokwi simulator connects through <code>ws://host.wokwi.internal:8080</code>, which counts as this PC.
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
