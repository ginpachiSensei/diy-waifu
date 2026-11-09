import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

interface EspData {
  emotion?: string;
  text?: string;
}

type ModelStatus = "not_installed" | "installing" | "installed" | "uninstalling";

function App() {
  // --- Chat State ---
  const [chatInput, setChatInput] = useState("");
  const [chatLog, setChatLog] = useState<{ from: "you" | "companion"; text: string }[]>([]);
  const [chatError, setChatError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);

  // --- WebSocket State ---
  const [serverStatus, setServerStatus] = useState<string>("starting...");
  const [lastMessage, setLastMessage] = useState<EspData | null>(null);
  const [clients, setClients] = useState<string[]>([]);
  const [phoneUrl, setPhoneUrl] = useState<string | null>(null);

  // --- Model/Companion State ---
  const [modelStatus, setModelStatus] = useState<ModelStatus>("not_installed");
  const [isPaused, setIsPaused] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    // 1. WebSocket Listeners
    const unlistenServer = listen<string>("ws:server_status", (payload) => {
      setServerStatus(payload.payload);
    });
    const unlistenMsg = listen<EspData>("ws:message", (payload) => {
      setLastMessage(payload.payload);
    });
    const unlistenClientConn = listen<string>("ws:client_connected", (payload) => {
      setClients((prev) => [...prev, payload.payload]);
    });
    const unlistenClientDisconn = listen<string>("ws:client_disconnected", (payload) => {
      setClients((prev) => prev.filter((c) => c !== payload.payload));
    });

    // 2. Model Listeners
    const unlistenModelStatus = listen<ModelStatus>("model:status", (payload) => {
      setModelStatus(payload.payload);
    });
    const unlistenCompanion = listen<boolean>("companion:paused", (payload) => {
      setIsPaused(payload.payload);
    });

    // Check current status on load
    invoke<boolean>("check_model_status")
      .then((installed) => {
        setModelStatus(installed ? "installed" : "not_installed");
      })
      .catch(console.error);

    invoke<string>("get_phone_url").then(setPhoneUrl).catch(console.error);

    // Cleanup listeners on unmount
    return () => {
      unlistenServer.then((f) => f());
      unlistenMsg.then((f) => f());
      unlistenClientConn.then((f) => f());
      unlistenClientDisconn.then((f) => f());
      unlistenModelStatus.then((f) => f());
      unlistenCompanion.then((f) => f());
    };
  }, []);

  // --- Handlers ---
  async function handleInstall() {
    setBusy(true);
    try {
      await invoke("install_model");
    } catch (e) {
      console.error(e);
    }
    setBusy(false);
  }

  async function handleUninstall() {
    setBusy(true);
    try {
      await invoke("uninstall_model");
    } catch (e) {
      console.error(e);
    }
    setBusy(false);
  }

  async function togglePause() {
    const next = !isPaused;
    try {
      await invoke("set_paused", { paused: next });
      setIsPaused(next);
    } catch (e) {
      console.error(e);
    }
  }

  // The reply also goes out over the WebSocket, so the ESP32 shows it at the same time
  async function sendChat() {
    const text = chatInput.trim();
    if (!text || sending) return;
    setChatInput("");
    setChatError(null);
    setChatLog((prev) => [...prev, { from: "you", text }]);
    setSending(true);
    try {
      const reply = await invoke<string>("send_chat_message", { text });
      setChatLog((prev) => [...prev, { from: "companion", text: reply }]);
    } catch (e) {
      setChatError(String(e));
    }
    setSending(false);
  }

  return (
    <main className="container">
      <h1>🧠 DIY Waifu Brain</h1>

      {/* Model Management Panel */}
      <div className="status-panel">
        <h2>Companion Model</h2>
        <p>
          Status:{" "}
          <span
            style={{
              fontWeight: "bold",
              color:
                modelStatus === "installed"
                  ? "#4caf50"
                  : modelStatus === "not_installed"
                  ? "#f44336"
                  : "#ff9800",
            }}
          >
            {modelStatus === "installed" && "✅ Installed"}
            {modelStatus === "not_installed" && "⛔ Not Installed"}
            {modelStatus === "installing" && "⏳ Installing..."}
            {modelStatus === "uninstalling" && "⏳ Removing..."}
          </span>
        </p>

        <div className="row" style={{ gap: "0.5rem" }}>
          {modelStatus === "not_installed" && (
            <button onClick={handleInstall} disabled={busy}>
              📥 Install Model
            </button>
          )}
          {modelStatus === "installed" && (
            <button onClick={handleUninstall} disabled={busy}>
              🗑️ Uninstall Model
            </button>
          )}
          <button
            onClick={togglePause}
            disabled={modelStatus !== "installed"}
            style={{ background: isPaused ? "#ff9800" : undefined }}
          >
            {isPaused ? "▶️ Resume Companion" : "⏸️ Pause Companion"}
          </button>
        </div>

        {isPaused && (
          <p className="placeholder" style={{ marginTop: "1rem" }}>
            Companion is paused — chat requests will be rejected until resumed.
          </p>
        )}
      </div>

      {/* WebSocket Server Status */}
      <div className="status-panel">
        <h2>WebSocket Server</h2>
        <p>
          Status:{" "}
          <span style={{ color: serverStatus === "connected" ? "#4caf50" : "#ff9800", fontWeight: "bold" }}>
            {serverStatus === "connected" ? "✅ Running" : "⏳ Starting..."}
          </span>
        </p>
        <p>
          Address: <code>ws://&lt;this PC&gt;:8080</code> (Wokwi: <code>ws://host.wokwi.internal:8080</code>)
        </p>
        <p>
          Connected Clients: <strong>{clients.length}</strong>
        </p>
      </div>

      {/* Phone body */}
      <div className="status-panel">
        <h2>Phone Body</h2>
        <p>
          Open this on an Android phone on the same Wi-Fi:{" "}
          {phoneUrl ? <code>{phoneUrl}</code> : <span className="placeholder">finding address...</span>}
        </p>
        <p className="placeholder">Works alongside the ESP32. Every connected body shows every reply.</p>
      </div>

      {/* Latest ESP32 Message */}
      <div className="status-panel">
        <h2>Latest Body Message</h2>
        {lastMessage ? (
          <div className="message-display">
            {lastMessage.emotion && (
              <p>
                <strong>Emotion:</strong> <span className="emotion">{lastMessage.emotion}</span>
              </p>
            )}
            {lastMessage.text && (
              <p>
                <strong>Text:</strong> "{lastMessage.text}"
              </p>
            )}
          </div>
        ) : (
          <p className="placeholder">Waiting for messages from the ESP32 or phone...</p>
        )}
      </div>

      {/* Chat */}
      <div className="status-panel">
        <h2>Chat</h2>
        {chatLog.length === 0 && <p className="placeholder">Say hi — replies also show on the ESP32 and phone screens.</p>}
        {chatLog.map((m, i) => (
          <p key={i}>
            <strong>{m.from === "you" ? "You" : "Companion"}:</strong> {m.text}
          </p>
        ))}
        {sending && <p className="placeholder">Thinking...</p>}
        {chatError && <p style={{ color: "#f44336" }}>{chatError}</p>}
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            sendChat();
          }}
        >
          <input
            value={chatInput}
            onChange={(e) => setChatInput(e.currentTarget.value)}
            placeholder="Type a message..."
            disabled={sending}
          />
          <button type="submit" disabled={sending || !chatInput.trim()}>
            Send
          </button>
        </form>
      </div>
    </main>
  );
}

export default App;
