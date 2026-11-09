import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ReplyLength, Settings, errorText } from "../api";

interface Props {
  settings: Settings | null;
  onSaved: (s: Settings) => void;
}

export default function PersonaTab({ settings, onSaved }: Props) {
  const [draft, setDraft] = useState<Settings | null>(settings);
  const [status, setStatus] = useState<string | null>(null);

  // Pick up changes saved elsewhere (e.g. the Model tab)
  useEffect(() => setDraft(settings), [settings]);

  if (!draft) return <p className="muted">Loading…</p>;

  const set = <K extends keyof Settings>(key: K, value: Settings[K]) => {
    setDraft({ ...draft, [key]: value });
    setStatus(null);
  };
  const dirty = JSON.stringify(draft) !== JSON.stringify(settings);

  async function save() {
    try {
      const saved = await invoke<Settings>("save_settings", { settings: draft });
      onSaved(saved);
      setStatus("Saved. Applies from the next message.");
    } catch (e) {
      setStatus(errorText(e));
    }
  }

  return (
    <form
      className="form"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <label>
        Companion's name
        <input value={draft.companionName} onChange={(e) => set("companionName", e.currentTarget.value)} />
      </label>
      <label>
        Your name
        <input
          value={draft.userName}
          placeholder="Optional"
          onChange={(e) => set("userName", e.currentTarget.value)}
        />
      </label>
      <label>
        Personality
        <textarea
          rows={5}
          value={draft.personality}
          onChange={(e) => set("personality", e.currentTarget.value)}
        />
        <span className="hint">Written to the companion, e.g. "You are shy but witty and love space facts."</span>
      </label>
      <label>
        Reply length
        <select
          value={draft.replyLength}
          onChange={(e) => set("replyLength", e.currentTarget.value as ReplyLength)}
        >
          <option value="short">Short (1-2 sentences)</option>
          <option value="medium">Medium (2-4 sentences)</option>
          <option value="long">Long (a short paragraph)</option>
        </select>
      </label>
      <div className="row">
        <button type="submit" className="primary" disabled={!dirty}>
          Save
        </button>
        <button type="button" disabled={!dirty} onClick={() => setDraft(settings)}>
          Undo changes
        </button>
        {status && <span className="muted">{status}</span>}
      </div>
    </form>
  );
}
