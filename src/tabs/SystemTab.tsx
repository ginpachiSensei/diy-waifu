import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { DesktopState, OllamaStatus, PauseState, Settings, errorText } from "../api";

interface Props {
  ollamaStatus: OllamaStatus;
  pause: PauseState;
  toggleManualPause: () => void;
  settings: Settings | null;
  onSaved: (s: Settings) => void;
}

export default function SystemTab({ ollamaStatus, pause, toggleManualPause, settings, onSaved }: Props) {
  const [desktop, setDesktop] = useState<DesktopState | null>(null);
  const [heavyText, setHeavyText] = useState("");
  const [error, setError] = useState<string | null>(null);

  const refresh = () => invoke<DesktopState>("get_desktop_state").then(setDesktop).catch(console.error);

  useEffect(() => {
    refresh();
    // The tray can change these too
    const unlisten = [listen("settings:changed", refresh), listen("pet:click_through", refresh)];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, []);

  useEffect(() => {
    if (settings) setHeavyText(settings.heavyApps.join("\n"));
  }, [settings]);

  async function run(cmd: string, args: Record<string, unknown>) {
    setError(null);
    try {
      await invoke(cmd, args);
    } catch (e) {
      setError(errorText(e));
    }
    refresh();
  }

  async function saveSettings(patch: Partial<Settings>) {
    if (!settings) return;
    setError(null);
    try {
      onSaved(await invoke<Settings>("save_settings", { settings: { ...settings, ...patch } }));
    } catch (e) {
      setError(errorText(e));
    }
  }

  const heavyList = heavyText
    .split(/[\n,]/)
    .map((s) => s.trim().toLowerCase())
    .filter(Boolean);
  const heavyDirty = settings != null && heavyList.join("\n") !== [...settings.heavyApps].sort().join("\n");

  return (
    <div className="stack">
      {error && <p className="error">{error}</p>}

      <section>
        <h3>Desktop pet</h3>
        <label className="check">
          <input
            type="checkbox"
            checked={desktop?.petVisible ?? false}
            onChange={(e) => run("set_pet_visible", { visible: e.currentTarget.checked })}
          />
          Show the pet on the desktop
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={desktop?.clickThrough ?? false}
            onChange={(e) => run("set_pet_click_through", { on: e.currentTarget.checked })}
          />
          Click-through (clicks go to the window underneath; turn off here or in the tray)
        </label>
        <p className="hint">Drag the pet by its body. Hover it for the chat and panel buttons.</p>
      </section>

      <section>
        <h3>Start with Windows</h3>
        <label className="check">
          <input
            type="checkbox"
            checked={desktop?.autostart ?? false}
            onChange={(e) => run("set_autostart", { enabled: e.currentTarget.checked })}
          />
          Start when I sign in, in the tray with just the pet showing
        </label>
        <p className="hint">
          Closing this window keeps the companion running in the tray. Use Quit in the tray menu to stop it. In dev
          builds this registers the debug exe, so turn it off before you uninstall or move the project.
        </p>
      </section>

      <section>
        <h3>Pause</h3>
        <p>
          {pause.manual
            ? "Paused by you."
            : pause.autoReason
              ? `Auto-paused: ${pause.autoReason}.`
              : "Running."}{" "}
          Pausing frees the model's memory and turns away new messages.
        </p>
        <button className={pause.manual ? "primary" : ""} onClick={toggleManualPause}>
          {pause.manual ? "Resume" : "Pause"}
        </button>
      </section>

      <section>
        <h3>Auto-pause</h3>
        <label className="check">
          <input
            type="checkbox"
            checked={settings?.autoPause ?? false}
            onChange={(e) => saveSettings({ autoPause: e.currentTarget.checked })}
          />
          Pause while any of these apps is running
        </label>
        <textarea
          rows={4}
          value={heavyText}
          onChange={(e) => setHeavyText(e.currentTarget.value)}
          placeholder="blender.exe"
          disabled={!settings?.autoPause}
        />
        <div className="row">
          <button disabled={!heavyDirty} onClick={() => saveSettings({ heavyApps: heavyList })}>
            Save list
          </button>
          <span className="hint">One process name per line, as shown in Task Manager's Details tab.</span>
        </div>
      </section>

      <section>
        <h3>Ollama</h3>
        <p>
          {ollamaStatus === "ready" && <span className="ok">running</span>}
          {ollamaStatus === "starting" && <span className="muted">starting…</span>}
          {ollamaStatus === "failed" && <span className="error">not responding, retrying</span>}
        </p>
        <p className="hint">
          It restarts on its own if it stops responding. Logs are in{" "}
          <code>%LOCALAPPDATA%\com.satyam.diy-waifu\logs</code>, chat history in{" "}
          <code>%APPDATA%\com.satyam.diy-waifu\companion.db</code>.
        </p>
      </section>
    </div>
  );
}
