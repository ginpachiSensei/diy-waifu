import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Settings, VoiceProgress, VoiceState, VoiceStatus, errorText } from "../api";
import { canSpeak, loadVoices, speak } from "../speech";

interface Props {
  settings: Settings | null;
  onSaved: (s: Settings) => void;
}

const STATE_TEXT: Record<string, string> = {
  idle: "Ready.",
  listening: "Listening…",
  transcribing: "Working out what you said…",
};

export default function VoiceTab({ settings, onSaved }: Props) {
  const [status, setStatus] = useState<VoiceStatus | null>(null);
  const [state, setState] = useState<VoiceState>({ state: "idle", error: null });
  const [progress, setProgress] = useState<VoiceProgress | null>(null);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [voices, setVoices] = useState<SpeechSynthesisVoice[]>([]);
  const [shortcut, setShortcut] = useState("");
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const [heard, setHeard] = useState<string | null>(null);

  const refresh = useCallback(() => {
    invoke<VoiceStatus>("voice_status").then(setStatus).catch(console.error);
  }, []);

  useEffect(() => {
    refresh();
    loadVoices().then(setVoices);
    const unlisten = [
      listen<VoiceState>("voice:state", (e) => setState(e.payload)),
      listen<VoiceProgress>("voice:progress", (e) => setProgress(e.payload)),
      listen<string>("voice:transcript", (e) => setHeard(e.payload)),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, [refresh]);

  useEffect(() => {
    if (settings) setShortcut(settings.pttShortcut);
  }, [settings]);

  if (!settings) return <p className="muted">Loading…</p>;

  async function save(patch: Partial<Settings>, ok?: string) {
    if (!settings) return;
    setMessage(null);
    try {
      onSaved(await invoke<Settings>("save_settings", { settings: { ...settings, ...patch } }));
      if (ok) setMessage({ ok: true, text: ok });
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
  }

  async function download(id: string) {
    setDownloading(id);
    setProgress(null);
    setMessage(null);
    try {
      await invoke("download_voice_model", { id });
      // First model: start using it
      if (!status?.models.some((m) => m.downloaded)) await save({ whisperModel: id });
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
    setDownloading(null);
    refresh();
  }

  async function remove(id: string) {
    try {
      await invoke("delete_voice_model", { id });
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
    refresh();
  }

  const selected = status?.models.find((m) => m.id === settings.whisperModel);
  const pct = progress?.total ? Math.round((progress.completed / progress.total) * 100) : null;

  return (
    <div className="stack">
      {message && <p className={message.ok ? "ok" : "error"}>{message.text}</p>}

      <section>
        <h3>Talk to it</h3>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.voiceInput}
            onChange={(e) => save({ voiceInput: e.currentTarget.checked })}
          />
          Push-to-talk: hold a key (or the 🎤 on the pet) and speak
        </label>
        <p>
          {state.state === "error" ? (
            <span className="error">{state.error}</span>
          ) : (
            <span className={state.state === "idle" ? "muted" : "ok"}>{STATE_TEXT[state.state]}</span>
          )}
          {heard && state.state === "idle" && <span className="muted"> Last heard: "{heard}"</span>}
        </p>
        <p className="hint">
          Microphone: {status?.microphone ?? "none found"}. Speech is turned into text on this PC by whisper.cpp;
          nothing is sent online.
        </p>
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            save({ pttShortcut: shortcut.trim() }, `Hold ${shortcut.trim()} to talk.`);
          }}
        >
          <label className="inline">
            Talk key
            <input value={shortcut} onChange={(e) => setShortcut(e.currentTarget.value)} placeholder="Ctrl+Shift+Space" />
          </label>
          <button type="submit" disabled={shortcut.trim() === settings.pttShortcut}>
            Save
          </button>
        </form>
        <p className="hint">Combinations like Ctrl+Shift+Space, Alt+Q or F8. It works in any app.</p>
      </section>

      <section>
        <h3>Speech model</h3>
        {!selected?.downloaded && (
          <p className="error">Download a model below to start talking. base.en is a good start.</p>
        )}
        <ul className="list">
          {status?.models.map((m) => (
            <li key={m.id}>
              <span className="grow">
                <span>
                  <code>{m.id}</code> <span className="muted">{m.sizeMb} MB</span>
                  {m.id === settings.whisperModel && m.downloaded && <span className="tag">in use</span>}
                  <br />
                  <span className="hint">{m.note}</span>
                </span>
              </span>
              <span className="row">
                {m.downloaded ? (
                  <>
                    {m.id !== settings.whisperModel && <button onClick={() => save({ whisperModel: m.id })}>Use</button>}
                    <button onClick={() => remove(m.id)}>Delete</button>
                  </>
                ) : (
                  <button onClick={() => download(m.id)} disabled={!!downloading}>
                    {downloading === m.id ? `${pct ?? 0}%` : "Download"}
                  </button>
                )}
              </span>
            </li>
          ))}
        </ul>
        {downloading && (
          <div className="progress">
            <div style={{ width: `${pct ?? 0}%` }} />
          </div>
        )}
      </section>

      <section>
        <h3>Hear it talk</h3>
        {!canSpeak ? (
          <p className="muted">Voices aren't available in this window.</p>
        ) : (
          <>
            <label className="check">
              <input
                type="checkbox"
                checked={settings.speakReplies}
                onChange={(e) => save({ speakReplies: e.currentTarget.checked })}
              />
              Read replies aloud from the pet
            </label>
            <div className="row">
              <label className="inline">
                Voice
                <select value={settings.ttsVoice} onChange={(e) => save({ ttsVoice: e.currentTarget.value })}>
                  <option value="">Windows default</option>
                  {voices.map((v) => (
                    <option key={v.name} value={v.name}>
                      {v.name} ({v.lang})
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <div className="row">
              <label className="inline">
                Speed
                <input
                  type="range"
                  min={0.5}
                  max={2}
                  step={0.1}
                  value={settings.ttsRate}
                  onChange={(e) => save({ ttsRate: Number(e.currentTarget.value) })}
                />
                {settings.ttsRate.toFixed(1)}×
              </label>
              <label className="inline">
                Pitch
                <input
                  type="range"
                  min={0.5}
                  max={2}
                  step={0.1}
                  value={settings.ttsPitch}
                  onChange={(e) => save({ ttsPitch: Number(e.currentTarget.value) })}
                />
                {settings.ttsPitch.toFixed(1)}
              </label>
              <button onClick={() => speak(`Hi! I'm ${settings.companionName}. Does this voice sound okay?`, settings)}>
                Test
              </button>
            </div>
            <p className="hint">
              These are the voices installed in Windows (Settings → Time &amp; language → Speech adds more). Nicer
              offline voices (Piper) can come later.
            </p>
          </>
        )}
      </section>
    </div>
  );
}
