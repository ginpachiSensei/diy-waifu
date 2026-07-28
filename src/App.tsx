import { useState, useEffect } from "react";
import reactLogo from "./assets/react.svg";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

interface EspData {
  emotion?: string;
  text?: string;
}

type ModelStatus = "not_installed" | "installing" | "installed" | "uninstalling";

function App() {
  // --- Boilerplate State ---
  const [greetMsg, setGreetMsg] = useState("");
  const [name, setName] = useState("");

  // --- WebSocket State ---
  const [serverStatus, setServerStatus] = useState<string>("starting...");
  const [lastMessage, setLastMessage] = useState<EspData | null>(null);
  const [clients, setClients] = useState<string[]>([]);

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

  async function greet() {
    setGreetMsg(await invoke("greet", { name }));
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
          Address: <code>ws://127.0.0.1:8080</code>
        </p>
        <p>
          Connected Clients: <strong>{clients.length}</strong>
        </p>
      </div>

      {/* Latest ESP32 Message */}
      <div className="status-panel">
        <h2>Latest ESP32 Message</h2>
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
          <p className="placeholder">Waiting for messages from ESP32...</p>
        )}
      </div>

      {/* Boilerplate Vite/Tauri/React links */}
      <div className="row">
        <a href="https://vite.dev" target="_blank">
          <img src="/vite.svg" className="logo vite" alt="Vite logo" />
        </a>
        <a href="https://tauri.app" target="_blank">
          <img src="/tauri.svg" className="logo tauri" alt="Tauri logo" />
        </a>
        <a href="https://react.dev" target="_blank">
          <img src={reactLogo} className="logo react" alt="React logo" />
        </a>
      </div>
      <p>Click on the Tauri, Vite, and React logos to learn more.</p>

      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          greet();
        }}
      >
        <input
          id="greet-input"
          onChange={(e) => setName(e.currentTarget.value)}
          placeholder="Enter a name..."
        />
        <button type="submit">Greet</button>
      </form>
      <p>{greetMsg}</p>
    </main>
  );
}

export default App;
