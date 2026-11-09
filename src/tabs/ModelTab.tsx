import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  InstalledModel,
  OllamaStatus,
  PullProgress,
  RECOMMENDED_MODELS,
  Settings,
  errorText,
  formatBytes,
  sameModel,
} from "../api";

interface Props {
  settings: Settings | null;
  models: InstalledModel[];
  ollamaStatus: OllamaStatus;
  refreshModels: () => Promise<InstalledModel[]>;
  onSaved: (s: Settings) => void;
}

export default function ModelTab({ settings, models, ollamaStatus, refreshModels, onSaved }: Props) {
  const [pulling, setPulling] = useState<string | null>(null);
  const [progress, setProgress] = useState<PullProgress | null>(null);
  const [custom, setCustom] = useState("");
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const u = listen<PullProgress>("model:progress", (e) => setProgress(e.payload));
    return () => {
      u.then((f) => f());
    };
  }, []);

  const ready = ollamaStatus === "ready";
  const isInstalled = (name: string) => models.some((m) => sameModel(m.name, name));

  async function use(model: string) {
    if (!settings) return;
    setError(null);
    try {
      onSaved(await invoke<Settings>("save_settings", { settings: { ...settings, model } }));
    } catch (e) {
      setError(errorText(e));
    }
  }

  async function pull(model: string) {
    model = model.trim();
    if (!model || pulling) return;
    setError(null);
    setPulling(model);
    setProgress(null);
    try {
      await invoke("pull_model", { model });
      const now = await refreshModels();
      // Nothing usable was selected before: switch to the new model
      if (settings && !now.some((m) => sameModel(m.name, settings.model))) await use(model);
      setCustom("");
    } catch (e) {
      setError(`Couldn't download ${model}: ${errorText(e)}`);
    }
    setPulling(null);
    setProgress(null);
  }

  async function remove(model: string) {
    if (confirmDelete !== model) {
      setConfirmDelete(model);
      setTimeout(() => setConfirmDelete((c) => (c === model ? null : c)), 3000);
      return;
    }
    setConfirmDelete(null);
    setError(null);
    try {
      await invoke("delete_model", { model });
      await refreshModels();
    } catch (e) {
      setError(errorText(e));
    }
  }

  const pct =
    progress?.total && progress.completed != null
      ? Math.round((progress.completed / progress.total) * 100)
      : null;

  return (
    <div className="stack">
      {!ready && (
        <p className={ollamaStatus === "failed" ? "error" : "muted"}>
          {ollamaStatus === "failed"
            ? "Ollama didn't start. Check the log, or restart the app."
            : "Starting Ollama…"}
        </p>
      )}

      <section>
        <h3>In use</h3>
        {settings && (
          <p>
            <code>{settings.model}</code>{" "}
            {ready &&
              (isInstalled(settings.model) ? (
                <span className="ok">downloaded</span>
              ) : (
                <span className="error">not downloaded yet, get it below</span>
              ))}
          </p>
        )}
      </section>

      {pulling && (
        <section className="card">
          <strong>Downloading {pulling}</strong>
          <div className="progress">
            <div style={{ width: `${pct ?? 0}%` }} />
          </div>
          <span className="muted">
            {progress?.status ?? "starting…"}
            {pct != null && ` · ${pct}%`}
            {progress?.total ? ` of ${formatBytes(progress.total)}` : ""}
          </span>
        </section>
      )}

      {error && <p className="error">{error}</p>}

      <section>
        <h3>Downloaded</h3>
        {models.length === 0 && <p className="muted">{ready ? "No models yet." : "…"}</p>}
        <ul className="list">
          {models.map((m) => {
            const current = settings ? sameModel(m.name, settings.model) : false;
            return (
              <li key={m.name}>
                <span>
                  <code>{m.name}</code> <span className="muted">{formatBytes(m.size)}</span>
                  {current && <span className="tag">in use</span>}
                </span>
                <span className="row">
                  {!current && <button onClick={() => use(m.name)}>Use</button>}
                  <button onClick={() => remove(m.name)}>{confirmDelete === m.name ? "Sure?" : "Delete"}</button>
                </span>
              </li>
            );
          })}
        </ul>
      </section>

      <section>
        <h3>Get a model</h3>
        <ul className="list">
          {RECOMMENDED_MODELS.filter((r) => !isInstalled(r.name)).map((r) => (
            <li key={r.name}>
              <span>
                <code>{r.name}</code> <span className="muted">{r.size}</span>
                <br />
                <span className="hint">{r.note}</span>
              </span>
              <button onClick={() => pull(r.name)} disabled={!ready || !!pulling}>
                Download
              </button>
            </li>
          ))}
        </ul>
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            pull(custom);
          }}
        >
          <input
            value={custom}
            onChange={(e) => setCustom(e.currentTarget.value)}
            placeholder="Any Ollama model, e.g. phi4-mini"
          />
          <button type="submit" disabled={!ready || !!pulling || !custom.trim()}>
            Download
          </button>
        </form>
      </section>
    </div>
  );
}
