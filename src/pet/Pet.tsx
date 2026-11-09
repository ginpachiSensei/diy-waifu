// The floating desktop pet. It's a body like the phone: it connects to the
// app's WebSocket as "pet", animates with the shared avatar state machine
// (phone/avatar.js), shows replies in a speech bubble, and has a quick chat box.
// Drag it by the GIF; the buttons appear on hover.

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createAvatar, gifFor, CompanionMessage, GifManifest } from "../../phone/avatar.js";

const WS_URL = "ws://127.0.0.1:8080";
const RECONNECT_MS = 3000;
// How long a finished reply stays up: at least 6s, longer for long replies
const bubbleHoldMs = (text: string) => Math.max(6000, text.length * 70);

type Bubble = { kind: "thinking" | "searching" | "talking" | "reply"; text: string } | null;

export default function Pet() {
  const [gifs, setGifs] = useState<{ base: string; manifest: GifManifest } | null>(null);
  const [state, setState] = useState("idle");
  const [avatar] = useState(() => createAvatar(setState));
  const [bubble, setBubble] = useState<Bubble>(null);
  const [connected, setConnected] = useState(false);
  const [chatOpen, setChatOpen] = useState(false);
  const [input, setInput] = useState("");
  const [retry, setRetry] = useState(0);
  const wsRef = useRef<WebSocket | null>(null);
  const hideTimer = useRef<number | undefined>(undefined);
  const bubbleRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    invoke<{ base: string; manifest: GifManifest }>("get_gif_manifest").then(setGifs).catch(console.error);
  }, []);

  // WebSocket with auto-reconnect
  useEffect(() => {
    let stopped = false;
    let reconnect: number | undefined;

    const onMessage = (msg: CompanionMessage) => {
      avatar.handle(msg);
      const type = msg.type || "reply";
      if (type === "user" || type === "thinking") {
        window.clearTimeout(hideTimer.current);
        setBubble({ kind: "thinking", text: "" });
      } else if (type === "tool") {
        window.clearTimeout(hideTimer.current);
        setBubble({ kind: "searching", text: `🔎 ${msg.label ?? "looking it up"}…` });
      } else if (type === "token") {
        window.clearTimeout(hideTimer.current);
        setBubble((b) => ({ kind: "talking", text: (b?.kind === "talking" ? b.text : "") + (msg.text ?? "") }));
      } else if (type === "reply") {
        const text = msg.text ?? "";
        setBubble({ kind: "reply", text });
        window.clearTimeout(hideTimer.current);
        hideTimer.current = window.setTimeout(() => setBubble(null), bubbleHoldMs(text));
      }
    };

    const connect = () => {
      if (stopped) return;
      const ws = new WebSocket(WS_URL);
      wsRef.current = ws;
      ws.onopen = () => {
        setConnected(true);
        ws.send(JSON.stringify({ type: "hello", body: "pet" }));
      };
      ws.onmessage = (e) => {
        try {
          const msg = JSON.parse(e.data);
          if (msg && typeof msg === "object") onMessage(msg);
        } catch {
          /* not JSON */
        }
      };
      ws.onclose = () => {
        setConnected(false);
        if (!stopped) reconnect = window.setTimeout(connect, RECONNECT_MS);
      };
      ws.onerror = () => ws.close();
    };
    connect();

    return () => {
      stopped = true;
      window.clearTimeout(reconnect);
      wsRef.current?.close();
    };
  }, [avatar]);

  // Keep the newest streamed text in view
  useEffect(() => {
    const el = bubbleRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [bubble]);

  useEffect(() => {
    if (chatOpen) inputRef.current?.focus();
  }, [chatOpen]);

  function send() {
    const text = input.trim();
    const ws = wsRef.current;
    if (!text || !ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ text }));
    setInput("");
    setChatOpen(false);
  }

  const gifName = gifs ? gifFor(gifs.manifest, state) : null;
  // `retry` only changes after a load error (the page server may start after this window)
  const gifSrc = gifs && gifName ? `${gifs.base}${gifName}${retry ? `?r=${retry}` : ""}` : null;

  return (
    <div className="pet">
      {bubble && (
        <div className={`bubble ${bubble.kind}`} ref={bubbleRef} onClick={() => setBubble(null)}>
          {bubble.kind === "thinking" ? (
            <span className="dots">
              <i />
              <i />
              <i />
            </span>
          ) : (
            bubble.text
          )}
        </div>
      )}

      {chatOpen && (
        <form
          className="quick-chat"
          onSubmit={(e) => {
            e.preventDefault();
            send();
          }}
        >
          <input
            ref={inputRef}
            value={input}
            placeholder={connected ? "Say something…" : "Not connected…"}
            onChange={(e) => setInput(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Escape" && setChatOpen(false)}
          />
        </form>
      )}

      <div className="figure">
        {gifSrc && (
          <img
            src={gifSrc}
            alt=""
            draggable={false}
            data-tauri-drag-region
            onError={() => window.setTimeout(() => setRetry((r) => r + 1), 1500)}
          />
        )}
        <div className="controls">
          <button title="Chat" onClick={() => setChatOpen((o) => !o)}>
            💬
          </button>
          <button title="Open panel" onClick={() => invoke("open_panel")}>
            ⚙
          </button>
        </div>
        {!connected && <span className="offline" title="Not connected to the companion app" />}
      </div>
    </div>
  );
}
