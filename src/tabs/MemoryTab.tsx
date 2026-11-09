import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Fact, MemoryStatus, PullProgress, Reminder, Settings, Summary, errorText } from "../api";

interface Props {
  settings: Settings | null;
  onSaved: (s: Settings) => void;
}

function when(ts: number) {
  const d = new Date(ts);
  const today = new Date();
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === today.toDateString()) return time;
  return `${d.toLocaleDateString([], { day: "numeric", month: "short" })} ${time}`;
}

export default function MemoryTab({ settings, onSaved }: Props) {
  const [status, setStatus] = useState<MemoryStatus | null>(null);
  const [facts, setFacts] = useState<Fact[]>([]);
  const [summaries, setSummaries] = useState<Summary[]>([]);
  const [reminders, setReminders] = useState<Reminder[]>([]);
  const [newFact, setNewFact] = useState("");
  const [editing, setEditing] = useState<{ id: number; text: string } | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [pulling, setPulling] = useState<PullProgress | null>(null);
  const [downloading, setDownloading] = useState(false);

  const refresh = useCallback(() => {
    invoke<MemoryStatus>("memory_status").then(setStatus).catch(console.error);
    invoke<Fact[]>("list_facts").then(setFacts).catch(console.error);
    invoke<Summary[]>("list_summaries").then(setSummaries).catch(console.error);
    invoke<Reminder[]>("list_reminders").then(setReminders).catch(console.error);
  }, []);

  useEffect(() => {
    refresh();
    const unlisten = [
      listen("memory:changed", refresh),
      listen("chat:message", refresh), // counts change
      listen<PullProgress>("model:progress", (e) => setPulling(e.payload)),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, [refresh]);

  async function run<T>(cmd: string, args: Record<string, unknown> = {}, ok?: string): Promise<T | undefined> {
    setMessage(null);
    try {
      const r = await invoke<T>(cmd, args);
      if (ok) setMessage({ ok: true, text: ok });
      refresh();
      return r;
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
      return undefined;
    }
  }

  async function downloadEmbedModel() {
    if (!status) return;
    setDownloading(true);
    await run("pull_model", { model: status.embedModel }, "Memory search model downloaded.");
    setDownloading(false);
    setPulling(null);
  }

  async function exportMemory() {
    const json = await run<string>("export_memory");
    if (!json) return;
    const url = URL.createObjectURL(new Blob([json], { type: "application/json" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `companion-memory-${new Date().toISOString().slice(0, 10)}.json`;
    a.click();
    URL.revokeObjectURL(url);
    setMessage({ ok: true, text: "Exported to your Downloads folder." });
  }

  async function importMemory(file: File) {
    const n = await run<number>("import_memory", { json: await file.text() });
    if (n !== undefined) setMessage({ ok: true, text: `Imported ${n} new memories.` });
  }

  async function clearAll() {
    if (!confirmClear) {
      setConfirmClear(true);
      setTimeout(() => setConfirmClear(false), 3000);
      return;
    }
    setConfirmClear(false);
    await run("clear_memory", {}, "Memory cleared. Chat history is still there.");
  }

  const pct =
    pulling?.total && pulling.completed != null ? Math.round((pulling.completed / pulling.total) * 100) : null;

  return (
    <div className="stack">
      {message && <p className={message.ok ? "ok" : "error"}>{message.text}</p>}

      <section>
        <h3>How it remembers</h3>
        <label className="check">
          <input
            type="checkbox"
            checked={settings?.memoryLearning ?? false}
            onChange={async (e) => {
              if (!settings) return;
              const s = await run<Settings>("save_settings", {
                settings: { ...settings, memoryLearning: e.currentTarget.checked },
              });
              if (s) onSaved(s);
            }}
          />
          Learn things about me from our chats (checked every few messages, in the background)
        </label>
        <p className="hint">
          You can also say "remember that …" or "forget …" in any chat, or edit the list below. Older chats are
          summarized automatically.
        </p>
        {status && !status.embedReady && (
          <div className="card">
            <span>
              The memory search model (<code>{status.embedModel}</code>, about 270 MB) isn't downloaded. Memories still
              work, but finding relevant old chats by meaning is off.
            </span>
            {downloading && pulling && (
              <>
                <div className="progress">
                  <div style={{ width: `${pct ?? 0}%` }} />
                </div>
                <span className="muted">
                  {pulling.status}
                  {pct != null && ` · ${pct}%`}
                </span>
              </>
            )}
            <div className="row">
              <button className="primary" onClick={downloadEmbedModel} disabled={downloading}>
                {downloading ? "Downloading…" : "Download"}
              </button>
            </div>
          </div>
        )}
        {status && (
          <p className="hint">
            {status.facts} memories · {status.summaries} summaries · {status.embeddedMessages} of {status.messages}{" "}
            messages searchable
          </p>
        )}
      </section>

      <section>
        <h3>Things it remembers</h3>
        <form
          className="row"
          onSubmit={async (e) => {
            e.preventDefault();
            if (await run("add_fact", { text: newFact }, "Saved.") !== undefined) setNewFact("");
          }}
        >
          <input
            className="grow-input"
            value={newFact}
            onChange={(e) => setNewFact(e.currentTarget.value)}
            placeholder="e.g. The user's exam is on 20 November"
          />
          <button type="submit" disabled={!newFact.trim()}>
            Add
          </button>
        </form>
        {facts.length === 0 ? (
          <p className="muted">Nothing yet.</p>
        ) : (
          <ul className="list">
            {facts.map((f) => (
              <li key={f.id}>
                {editing?.id === f.id ? (
                  <form
                    className="row grow"
                    onSubmit={async (e) => {
                      e.preventDefault();
                      await run("update_fact", { id: f.id, text: editing.text });
                      setEditing(null);
                    }}
                  >
                    <input
                      className="grow-input"
                      autoFocus
                      value={editing.text}
                      onChange={(e) => setEditing({ id: f.id, text: e.currentTarget.value })}
                      onKeyDown={(e) => e.key === "Escape" && setEditing(null)}
                    />
                    <button type="submit">Save</button>
                  </form>
                ) : (
                  <span className="grow">
                    {f.text}
                    {f.origin === "user" && <span className="tag">told</span>}
                  </span>
                )}
                {editing?.id !== f.id && (
                  <span className="row">
                    <button onClick={() => setEditing({ id: f.id, text: f.text })}>Edit</button>
                    <button onClick={() => run("delete_fact", { id: f.id })}>✕</button>
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h3>Reminders</h3>
        {reminders.length === 0 ? (
          <p className="muted">None. Try "remind me in 20 minutes to stretch".</p>
        ) : (
          <ul className="list">
            {reminders.map((r) => (
              <li key={r.id}>
                <span className="grow">
                  <strong>{when(r.dueTs)}</strong> {r.text}
                </span>
                <button onClick={() => run("delete_reminder", { id: r.id })}>Cancel</button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h3>Summaries of older chats</h3>
        {summaries.length === 0 ? (
          <p className="muted">None yet. They're written once a chat is long enough to scroll out of view.</p>
        ) : (
          <ul className="list">
            {summaries.map((s) => (
              <li key={s.id}>
                <span className="grow">
                  <span className="muted">{when(s.ts)}</span> {s.text}
                </span>
                <button onClick={() => run("delete_summary", { id: s.id })}>✕</button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h3>Backup</h3>
        <div className="row">
          <button onClick={exportMemory}>Export</button>
          <label className="button-like">
            Import
            <input
              type="file"
              accept="application/json,.json"
              hidden
              onChange={(e) => {
                const f = e.currentTarget.files?.[0];
                if (f) importMemory(f);
                e.currentTarget.value = "";
              }}
            />
          </label>
          <button onClick={clearAll}>{confirmClear ? "Sure? Click again" : "Clear all memory"}</button>
        </div>
        <p className="hint">Export saves memories and summaries as JSON. Clearing keeps your chat history.</p>
      </section>
    </div>
  );
}
