import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { listen } from "@tauri-apps/api/event";
import { ChatMessage, Settings, errorText } from "../api";

const SOURCE_LABEL: Record<string, string> = {
  phone: "via phone",
  esp32: "via ESP32",
  pet: "via pet",
  voice: "spoken",
  body: "via body",
};

export default function ChatTab({ settings }: { settings: Settings | null }) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [streaming, setStreaming] = useState<string | null>(null);
  const [lookup, setLookup] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [question, setQuestion] = useState<{ id: number; text: string } | null>(null);
  const logRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const add = (m: ChatMessage) =>
      setMessages((prev) => (prev.some((p) => p.id === m.id) ? prev : [...prev, m]));

    invoke<ChatMessage[]>("get_history", { limit: 200 })
      .then((history) =>
        // Keep anything that arrived by event while history was loading
        setMessages((prev) => [...history, ...prev.filter((p) => !history.some((h) => h.id === p.id))]),
      )
      .catch((e) => setError(errorText(e)));

    const unlisten = [
      listen<ChatMessage>("chat:message", (e) => {
        add(e.payload);
        if (e.payload.role === "user") setError(null);
      }),
      listen<boolean>("chat:busy", (e) => {
        setStreaming(e.payload ? "" : null);
        setLookup(null);
      }),
      listen<string>("chat:token", (e) => {
        setLookup(null);
        setStreaming((s) => (s ?? "") + e.payload);
      }),
      // A lookup started: drop any text streamed before it
      listen<string>("chat:tool", (e) => {
        setLookup(e.payload);
        setStreaming("");
      }),
      listen<string>("chat:error", (e) => setError(e.payload)),
      // "Ask me first" mode
      listen<{ id: number; text: string }>("chat:ask", (e) => setQuestion(e.payload)),
      listen<number>("chat:ask_done", (e) => setQuestion((q) => (q?.id === e.payload ? null : q))),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, []);

  // Stay scrolled to the newest message
  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, streaming]);

  async function send() {
    const text = input.trim();
    if (!text) return;
    setInput("");
    setError(null);
    try {
      await invoke("send_chat_message", { text });
    } catch (e) {
      setError(errorText(e));
    }
  }

  async function clearHistory() {
    if (!confirmClear) {
      setConfirmClear(true);
      setTimeout(() => setConfirmClear(false), 3000);
      return;
    }
    setConfirmClear(false);
    try {
      await invoke("clear_history");
      setMessages([]);
    } catch (e) {
      setError(errorText(e));
    }
  }

  const name = settings?.companionName || "Companion";
  const busy = streaming !== null;

  return (
    <div className="chat">
      <div className="chat-log" ref={logRef}>
        {messages.length === 0 && !busy && (
          <p className="muted center">Say hi. Replies also show on the phone and ESP32 screens.</p>
        )}
        {messages.map((m) => (
          <div key={m.id} className={`bubble ${m.role}`}>
            <div className="bubble-meta">
              {m.role === "user" ? "You" : name}
              {m.role === "user" && SOURCE_LABEL[m.source] && <span> · {SOURCE_LABEL[m.source]}</span>}
              {m.role === "assistant" && m.source === "proactive" && <span> · on its own</span>}
              {m.role === "assistant" && m.emotion && <span className="emotion"> · {m.emotion}</span>}
            </div>
            <div className="bubble-text">{m.text}</div>
            {m.sources?.length > 0 && (
              <div className="sources">
                {m.sources.map((s) => (
                  <a
                    key={s.url}
                    href={s.url}
                    title={s.url}
                    onClick={(e) => {
                      e.preventDefault();
                      openUrl(s.url).catch(console.error);
                    }}
                  >
                    {s.title || s.url}
                  </a>
                ))}
              </div>
            )}
          </div>
        ))}
        {busy && (
          <div className="bubble assistant">
            <div className="bubble-meta">{name}</div>
            <div className="bubble-text">
              {streaming || <span className="muted">{lookup ? `🔎 ${lookup}…` : "thinking…"}</span>}
            </div>
          </div>
        )}
      </div>

      {question && (
        <div className="card row">
          <span className="grow">{question.text}</span>
          <button
            className="primary"
            onClick={() => invoke("answer_ask", { id: question.id, yes: true }).catch(console.error)}
          >
            Yes
          </button>
          <button onClick={() => invoke("answer_ask", { id: question.id, yes: false }).catch(console.error)}>
            No
          </button>
        </div>
      )}

      {error && <p className="error">{error}</p>}

      <form
        className="chat-bar"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <input
          value={input}
          onChange={(e) => setInput(e.currentTarget.value)}
          placeholder={`Message ${name}…`}
          autoFocus
        />
        <button type="submit" className="primary" disabled={!input.trim()}>
          Send
        </button>
        <button type="button" onClick={clearHistory} disabled={messages.length === 0}>
          {confirmClear ? "Sure?" : "Clear"}
        </button>
      </form>
    </div>
  );
}
