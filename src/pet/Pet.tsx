// The floating desktop pet. It's a body like the phone: it connects to the
// app's WebSocket as "pet", animates with the shared avatar state machine
// (phone/avatar.js), shows replies in a speech bubble, and has a quick chat box.
// Drag it by the GIF; the buttons appear on hover.

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Settings, VoiceState } from "../api";
import { ReplySpeaker } from "../speech";
import { createAvatar, gifFor, CompanionMessage, GifManifest } from "../../phone/avatar.js";

const WS_URL = "ws://127.0.0.1:8080";
const RECONNECT_MS = 3000;
// How long a finished reply stays up: at least 6s, longer for long replies
const bubbleHoldMs = (text: string) => Math.max(6000, text.length * 70);

type Bubble = { kind: "thinking" | "searching" | "talking" | "reply" | "ask"; text: string; id?: number } | null;

export default function Pet() {
  const [gifs, setGifs] = useState<{ base: string; manifest: GifManifest } | null>(null);
  const [state, setState] = useState("idle");
  const [avatar] = useState(() => createAvatar(setState));
  const [bubble, setBubble] = useState<Bubble>(null);
  const [connected, setConnected] = useState(false);
  const [chatOpen, setChatOpen] = useState(false);
  const [input, setInput] = useState("");
  const [retry, setRetry] = useState(0);
  const [voice, setVoice] = useState<VoiceState["state"]>("idle");
  const settingsRef = useRef<Settings | null>(null);
  // Speaks replies sentence by sentence; tells Rust while it talks, so hands-free
  // can hear the wake phrase over the pet's own voice and cut it off
  const [speaker] = useState(
    () =>
      new ReplySpeaker(
        () => settingsRef.current,
        (on) => invoke("set_speaking", { on }).catch(console.error),
      ),
  );
  const holdingMic = useRef(false);
  const wsRef = useRef<WebSocket | null>(null);
  const hideTimer = useRef<number | undefined>(undefined);
  const bubbleRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    invoke<{ base: string; manifest: GifManifest }>("get_gif_manifest").then(setGifs).catch(console.error);
    invoke<Settings>("get_settings")
      .then((s) => (settingsRef.current = s))
      .catch(console.error);
    const unlisten = [
      listen<Settings>("settings:changed", (e) => (settingsRef.current = e.payload)),
      listen<VoiceState>("voice:state", (e) => {
        setVoice(e.payload.state);
        if (e.payload.state === "listening") speaker.interrupt();
        if (e.payload.state === "error" && e.payload.error) {
          window.clearTimeout(hideTimer.current);
          setBubble({ kind: "reply", text: `🎤 ${e.payload.error}` });
          hideTimer.current = window.setTimeout(() => setBubble(null), 6000);
        }
      }),
      // The wake phrase was heard while the pet was talking
      listen("voice:interrupt", () => speaker.interrupt()),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, [speaker]);

  // WebSocket with auto-reconnect
  useEffect(() => {
    let stopped = false;
    let reconnect: number | undefined;

    const onMessage = (msg: CompanionMessage) => {
      avatar.handle(msg);
      const type = msg.type || "reply";
      if (type === "user" || type === "thinking") {
        speaker.reset();
        window.clearTimeout(hideTimer.current);
        setBubble({ kind: "thinking", text: "" });
      } else if (type === "tool") {
        window.clearTimeout(hideTimer.current);
        speaker.restart();
        setBubble({ kind: "searching", text: `🔎 ${msg.label ?? "looking it up"}…` });
      } else if (type === "ask") {
        // "Ask me first" mode: yes/no before a lookup
        window.clearTimeout(hideTimer.current);
        setBubble({ kind: "ask", text: msg.text ?? "", id: msg.id });
      } else if (type === "ask_done") {
        setBubble((b) => (b?.kind === "ask" && b.id === msg.id ? { kind: "thinking", text: "" } : b));
      } else if (type === "token") {
        window.clearTimeout(hideTimer.current);
        setBubble((b) => ({ kind: "talking", text: (b?.kind === "talking" ? b.text : "") + (msg.text ?? "") }));
        speaker.token(msg.text ?? "");
      } else if (type === "reply") {
        const text = msg.text ?? "";
        setBubble({ kind: "reply", text });
        window.clearTimeout(hideTimer.current);
        hideTimer.current = window.setTimeout(() => setBubble(null), bubbleHoldMs(text));
        speaker.finish(text);
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
  }, [avatar, speaker]);

  // Tell Rust where the visible parts are, so clicks elsewhere in this
  // see-through window reach the app behind it (desktop::pass_through_loop)
  const rootRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    let last = "";
    const report = () => {
      const rects = [...root.querySelectorAll(".figure img, .figure .controls, .bubble, .quick-chat")]
        .map((el) => el.getBoundingClientRect())
        .filter((r) => r.width > 0 && r.height > 0)
        .map((r) => ({ x: r.x, y: r.y, w: r.width, h: r.height }));
      const key = JSON.stringify(rects);
      if (key === last) return;
      last = key;
      invoke("set_pet_hit_rects", { rects }).catch(console.error);
    };
    const observer = new ResizeObserver(report);
    root.querySelectorAll("*").forEach((el) => observer.observe(el));
    report();
    // Layout changes with the bubble and chat box; also catch late image loads
    const timer = window.setInterval(report, 1000);
    return () => {
      observer.disconnect();
      window.clearInterval(timer);
    };
  }, [bubble, chatOpen, gifs, connected]);

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

  function answer(id: number | undefined, yes: boolean) {
    if (id === undefined) return;
    invoke("answer_ask", { id, yes }).catch(console.error);
    setBubble({ kind: "thinking", text: "" });
  }

  const gifName = gifs ? gifFor(gifs.manifest, state) : null;
  // `retry` only changes after a load error (the page server may start after this window)
  const gifSrc = gifs && gifName ? `${gifs.base}${gifName}${retry ? `?r=${retry}` : ""}` : null;

  return (
    <div className="pet" ref={rootRef}>
      {bubble && (
        <div
          className={`bubble ${bubble.kind}`}
          ref={bubbleRef}
          onClick={() => bubble.kind !== "ask" && setBubble(null)}
        >
          {bubble.kind === "ask" ? (
            <>
              {bubble.text}
              <span className="ask-buttons">
                <button onClick={() => answer(bubble.id, true)}>Yes</button>
                <button onClick={() => answer(bubble.id, false)}>No</button>
              </span>
            </>
          ) : bubble.kind === "thinking" ? (
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
          <button
            title="Hold to talk"
            className={voice === "listening" ? "on" : ""}
            onPointerDown={(e) => {
              e.currentTarget.setPointerCapture(e.pointerId);
              holdingMic.current = true;
              invoke("voice_start").catch(() => {
                /* shown through voice:state */
              });
            }}
            onPointerUp={() => {
              if (!holdingMic.current) return;
              holdingMic.current = false;
              invoke("voice_stop").catch(console.error);
            }}
          >
            🎤
          </button>
          <button title="Chat" onClick={() => setChatOpen((o) => !o)}>
            💬
          </button>
          <button title="Open panel" onClick={() => invoke("open_panel")}>
            ⚙
          </button>
        </div>
        {!connected && <span className="offline" title="Not connected to the companion app" />}
        {voice === "listening" && <span className="listening" title="Listening" />}
        {voice === "transcribing" && <span className="listening busy" title="Working out what you said" />}
      </div>
    </div>
  );
}
