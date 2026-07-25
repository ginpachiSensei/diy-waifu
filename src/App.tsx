
import { useState, useEffect } from "react";
import reactLogo from "./assets/react.svg";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

interface EspData {
  emotion?: string;
  text?: string;
}

function App() {
  const [greetMsg, setGreetMsg] = useState("");
  const [name, setName] = useState("");
  const [serverStatus, setServerStatus] = useState<string>("starting...");
  const [lastMessage, setLastMessage] = useState<EspData | null>(null);
  const [clients, setClients] = useState<string[]>([]);

  useEffect(() => {
    // Listen for WebSocket server status
    listen("ws:server_status", (payload) => {
      console.log("Server status:", payload);
      setServerStatus(payload.payload as string);
    });

    // Listen for incoming messages from ESP32
    listen<EspData>("ws:message", (payload) => {
      console.log("ESP32 message:", payload);
      setLastMessage(payload.payload);
    });

    // Listen for client connections
    listen<string>("ws:client_connected", (payload) => {
      console.log("Client connected:", payload);
      setClients((prev) => [...prev, payload.payload]);
    });

    // Listen for client disconnections
    listen<string>("ws:client_disconnected", (payload) => {
      console.log("Client disconnected:", payload);
      setClients((prev) => prev.filter((c) => c !== payload.payload));
    });

    return () => {
      // Cleanup listeners
    };
  }, []);

  async function greet() {
    // Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
    setGreetMsg(await invoke("greet", { name }));
  }

  return (
    <main className="container">
      <h1>🧠 DIY Waifu Brain</h1>

      {/* WebSocket Server Status */}
      <div className="status-panel">
        <h2>WebSocket Server</h2>
        <p>
          Status:{" "}
          <span style={{ color: serverStatus === "connected" ? "#4caf50" : "#ff9800" }}>
            {serverStatus === "connected" ? "✅ Running" : "⏳ Starting..."}
          </span>
        </p>
        <p>Address: <code>ws://127.0.0.1:8080</code></p>
        <p>Connected Clients: <strong>{clients.length}</strong></p>
      </div>

      {/* Latest ESP32 Message */}
      <div className="status-panel">
        <h2>Latest ESP32 Message</h2>
        {lastMessage ? (
          <div className="message-display">
            {lastMessage.emotion && (
              <p><strong>Emotion:</strong> <span className="emotion">{lastMessage.emotion}</span></p>
            )}
            {lastMessage.text && <p><strong>Text:</strong> "{lastMessage.text}"</p>}
          </div>
        ) : (
          <p className="placeholder">Waiting for messages from ESP32...</p>
        )}
      </div>

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
      <p>Click on the Tauri, Vite, and React logos to learn about.</p>

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
