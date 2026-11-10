import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { KokoroStatus, PiperStatus, Settings, StudioStatus, VoiceProgress, VoiceState, VoiceStatus, errorText } from "../api";
import { canSpeak, loadVoices, speak, speakOffline } from "../speech";

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
  const [wakeWord, setWakeWord] = useState("");

  const [piper, setPiper] = useState<PiperStatus | null>(null);
  const [kokoro, setKokoro] = useState<KokoroStatus | null>(null);
  const [studio, setStudio] = useState<StudioStatus | null>(null);
  const [style, setStyle] = useState("");

  const refresh = useCallback(() => {
    invoke<VoiceStatus>("voice_status").then(setStatus).catch(console.error);
    invoke<PiperStatus>("piper_status").then(setPiper).catch(console.error);
    invoke<KokoroStatus>("kokoro_status").then(setKokoro).catch(console.error);
    invoke<StudioStatus>("studio_status").then(setStudio).catch(console.error);
  }, []);

  useEffect(() => {
    refresh();
    loadVoices().then(setVoices);
    const unlisten = [
      listen<VoiceState>("voice:state", (e) => setState(e.payload)),
      listen<VoiceProgress>("voice:progress", (e) => setProgress(e.payload)),
      listen<string>("voice:transcript", (e) => setHeard(e.payload)),
      listen<boolean>("voice:hands_free", refresh),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, [refresh]);

  useEffect(() => {
    if (settings) {
      setShortcut(settings.pttShortcut);
      setWakeWord(settings.wakeWord);
      setStyle(settings.studioStyle);
    }
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

  // null = the Piper engine, else a voice id
  async function getPiper(id: string | null) {
    setDownloading(id ?? "piper");
    setProgress(null);
    setMessage(null);
    try {
      if (id === null) await invoke("download_piper_engine");
      else {
        if (!piper?.engineReady) await invoke("download_piper_engine");
        setProgress(null);
        await invoke("download_piper_voice", { id });
        // First voice: start using it
        if (!piper?.voices.some((v) => v.downloaded)) await save({ piperVoice: id });
      }
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
    setDownloading(null);
    refresh();
  }

  async function removePiperVoice(id: string) {
    try {
      await invoke("delete_piper_voice", { id });
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
    refresh();
  }

  async function getKokoro() {
    setDownloading("kokoro");
    setProgress(null);
    setMessage(null);
    try {
      await invoke("download_kokoro");
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
    setDownloading(null);
    refresh();
  }

  async function removeKokoro() {
    try {
      await invoke("delete_kokoro");
      if (settings?.ttsEngine === "kokoro") await save({ ttsEngine: "windows" });
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
    refresh();
  }

  async function getStudio() {
    setDownloading("studio");
    setProgress(null);
    setMessage(null);
    try {
      await invoke("download_studio");
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
    setDownloading(null);
    refresh();
  }

  async function removeStudio() {
    try {
      await invoke("delete_studio");
      if (settings?.ttsEngine === "studio") await save({ ttsEngine: "windows" });
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
    refresh();
  }

  async function test() {
    if (!settings) return;
    const line =
      settings.language === "ja"
        ? `こんにちは！${settings.companionName}だよ。この声、どうかな？気に入ってくれるといいな！`
        : `Hi! I'm ${settings.companionName}. Does this voice sound okay? I hope you like it!`;
    if (settings.ttsEngine !== "windows") {
      try {
        await speakOffline(settings.ttsEngine, line);
      } catch (e) {
        setMessage({ ok: false, text: errorText(e) });
      }
    } else {
      speak(line, settings);
    }
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
        <h3>Hands-free</h3>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.handsFree}
            disabled={!settings.voiceInput}
            onChange={async (e) => {
              await save({ handsFree: e.currentTarget.checked });
              window.setTimeout(refresh, 2500); // it starts within a couple of seconds
            }}
          />
          Listen for the wake phrase all the time: say "{status?.wakePhrase ?? "hey …"}, what's the weather?"
        </label>
        {settings.handsFree && (
          <p>
            {status?.handsFree ? (
              <span className="ok">Listening for "{status.wakePhrase}"</span>
            ) : (
              <span className="muted">
                Not listening right now (it pauses with the companion, and needs a speech model below).
              </span>
            )}
          </p>
        )}
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            save({ wakeWord: wakeWord.trim() }, "Wake phrase saved.").then(refresh);
          }}
        >
          <label className="inline">
            Wake phrase
            <input
              value={wakeWord}
              onChange={(e) => setWakeWord(e.currentTarget.value)}
              placeholder={`hey ${settings.companionName}`}
            />
          </label>
          <button type="submit" disabled={wakeWord.trim() === settings.wakeWord}>
            Save
          </button>
        </form>
        <p className="hint">
          The microphone stays on and everything said nearby is turned into text on this PC to look for the phrase.
          Anything without it is thrown away, never saved or logged. It uses some CPU while people talk. Say just the
          phrase and it waits a few seconds for the rest.
        </p>
        {settings.language === "ja" && (
          <p className="hint">
            For Japanese, write the wake phrase in Japanese, e.g. 「ねえ、マイ」; hiragana and katakana both match.
          </p>
        )}
      </section>

      <section>
        <h3>Speech model</h3>
        {!selected?.downloaded && (
          <p className="error">Download a model below to start talking. base.en is a good start.</p>
        )}
        {settings.language !== "auto" && settings.language !== "en" && settings.whisperModel.endsWith(".en") && (
          <p className="error">
            {settings.whisperModel} only understands English. For {settings.language === "ja" ? "Japanese" : "Hindi"}, use
            small (or base).
          </p>
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
            Voices
            <select
              value={settings.ttsEngine}
              onChange={(e) => save({ ttsEngine: e.currentTarget.value as Settings["ttsEngine"] })}
            >
              <option value="studio" disabled={!studio?.supported}>
                Studio (most lifelike, heavy, English + Japanese)
              </option>
              <option value="kokoro" disabled={!kokoro?.supported}>
                Kokoro (natural, light, English)
              </option>
              <option value="piper" disabled={!piper?.supported}>
                Piper (offline, has Hindi)
              </option>
              <option value="windows">Windows voices</option>
            </select>
          </label>
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
          <button onClick={test}>Test</button>
        </div>

        {settings.ttsEngine === "windows" ? (
          !canSpeak ? (
            <p className="muted">Voices aren't available in this window.</p>
          ) : (
            <>
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
              </div>
              <p className="hint">
                These are the voices installed in Windows (Settings → Time &amp; language → Speech adds more).
              </p>
            </>
          )
        ) : settings.ttsEngine === "studio" ? (
          <>
            <label className="check">
              <input
                type="checkbox"
                checked={settings.studioGpu}
                onChange={async (e) => {
                  await save({ studioGpu: e.currentTarget.checked });
                  refresh();
                }}
              />
              Use the graphics card (Vulkan: NVIDIA, AMD or Intel). Much faster; needs a few GB of video memory.
            </label>
            {studio?.engineReady && studio.modelReady ? (
              <>
                <div className="row">
                  <label className="inline">
                    Voice
                    <select value={settings.studioVoice} onChange={(e) => save({ studioVoice: e.currentTarget.value })}>
                      {studio.voices.map((v) => (
                        <option key={v.id} value={v.id}>
                          {v.id}: {v.note}
                        </option>
                      ))}
                    </select>
                  </label>
                  <button onClick={removeStudio}>Delete studio voices</button>
                </div>
                <form
                  className="row"
                  onSubmit={(e) => {
                    e.preventDefault();
                    save({ studioStyle: style.trim() }, "Speaking style saved.");
                  }}
                >
                  <label className="inline grow">
                    Speaking style
                    <input
                      value={style}
                      onChange={(e) => setStyle(e.currentTarget.value)}
                      placeholder="e.g. Speak softly and sleepily."
                    />
                  </label>
                  <button type="submit" disabled={style.trim() === settings.studioStyle}>
                    Save
                  </button>
                </form>
                <p className="hint">
                  Every voice speaks every language; the style is a plain description of how to talk. The first sentence
                  after a while waits for the model to load. Speed doesn't apply to these voices.
                  {!settings.studioGpu && " On the CPU these voices are slow; the graphics card option helps a lot."}
                </p>
              </>
            ) : (
              <div className="card">
                <span>
                  Qwen3-TTS, a large voice model (about {(((studio?.engineMb ?? 32) + (studio?.modelMb ?? 2817)) / 1000).toFixed(1)} GB to download, and about 4 GB of memory while talking). It
                  sounds the most lifelike, in English, Japanese and 8 more languages, and runs on this PC; nothing is
                  sent online. Until it's downloaded, replies use a Windows voice.
                </span>
                <div className="row">
                  <button className="primary" onClick={getStudio} disabled={!!downloading}>
                    {downloading === "studio"
                      ? `${pct ?? 0}%`
                      : studio?.modelReady
                        ? "Download the engine"
                        : "Download studio voices"}
                  </button>
                </div>
              </div>
            )}
          </>
        ) : settings.ttsEngine === "kokoro" ? (
          kokoro?.ready ? (
            <>
              <div className="row">
                <label className="inline">
                  Voice
                  <select value={settings.kokoroVoice} onChange={(e) => save({ kokoroVoice: e.currentTarget.value })}>
                    {kokoro.voices.map((v) => (
                      <option key={v.id} value={v.id}>
                        {v.id}: {v.note}
                      </option>
                    ))}
                  </select>
                </label>
                <button onClick={removeKokoro}>Delete Kokoro</button>
              </div>
              <p className="hint">
                English only (Japanese lines use a Japanese Windows voice). All voices are in the one download; switching
                is instant.
              </p>
            </>
          ) : (
            <div className="card">
              <span>
                Kokoro (about {kokoro?.sizeMb ?? 160} MB) runs on this PC; nothing is sent online. Until it's downloaded,
                replies use a Windows voice.
              </span>
              <div className="row">
                <button className="primary" onClick={getKokoro} disabled={!!downloading}>
                  {downloading === "kokoro" ? `${pct ?? 0}%` : "Download Kokoro"}
                </button>
              </div>
            </div>
          )
        ) : (
          <>
            {!piper?.engineReady && (
              <div className="card">
                <span>Piper (about 22 MB) runs on this PC; nothing is sent online.</span>
                <div className="row">
                  <button className="primary" onClick={() => getPiper(null)} disabled={!!downloading}>
                    {downloading === "piper" ? `${pct ?? 0}%` : "Download Piper"}
                  </button>
                </div>
              </div>
            )}
            <ul className="list">
              {piper?.voices.map((v) => (
                <li key={v.id}>
                  <span className="grow">
                    <span>
                      <code>{v.id}</code> <span className="muted">{v.sizeMb} MB</span>
                      {v.id === settings.piperVoice && v.downloaded && <span className="tag">in use</span>}
                      <br />
                      <span className="hint">{v.note}</span>
                    </span>
                  </span>
                  <span className="row">
                    {v.downloaded ? (
                      <>
                        {v.id !== settings.piperVoice && (
                          <button onClick={() => save({ piperVoice: v.id })}>Use</button>
                        )}
                        <button onClick={() => removePiperVoice(v.id)}>Delete</button>
                      </>
                    ) : (
                      <button onClick={() => getPiper(v.id)} disabled={!!downloading}>
                        {downloading === v.id ? `${pct ?? 0}%` : "Download"}
                      </button>
                    )}
                  </span>
                </li>
              ))}
            </ul>
            {piper && !piper.voices.some((v) => v.id === settings.piperVoice && v.downloaded) && (
              <p className="error">Download a voice to use Piper. Until then replies use a Windows voice.</p>
            )}
          </>
        )}
      </section>
    </div>
  );
}
