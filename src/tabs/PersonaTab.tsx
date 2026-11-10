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
        Language
        <select
          value={draft.language}
          onChange={(e) => set("language", e.currentTarget.value as Settings["language"])}
        >
          <option value="auto">Same as you write in</option>
          <option value="en">English</option>
          <option value="ja">Japanese (日本語)</option>
          <option value="hi">Hindi (हिन्दी)</option>
        </select>
        <span className="hint">
          What the companion replies in. Speech-to-text listens for it too (with a multilingual speech model). For
          Japanese, a bigger chat model than the default replies more naturally.
        </span>
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
      <fieldset className="group">
        <legend>Speaking first</legend>
        <label className="check">
          <input
            type="checkbox"
            checked={draft.proactive}
            onChange={(e) => set("proactive", e.currentTarget.checked)}
          />
          Say hi, welcome me back, suggest breaks, and check in after long silences
        </label>
        <div className="row">
          <label className="inline">
            At most once every
            <input
              type="number"
              min={5}
              max={1440}
              value={draft.proactiveGapMinutes}
              onChange={(e) => set("proactiveGapMinutes", Number(e.currentTarget.value))}
            />
            minutes
          </label>
          <label className="inline">
            Break nudge after
            <input
              type="number"
              min={15}
              max={720}
              value={draft.breakAfterMinutes}
              onChange={(e) => set("breakAfterMinutes", Number(e.currentTarget.value))}
            />
            minutes of coding or creating
          </label>
        </div>
        <div className="row">
          <label className="inline">
            Quiet from
            <input type="time" value={draft.quietStart} onChange={(e) => set("quietStart", e.currentTarget.value)} />
            to
            <input type="time" value={draft.quietEnd} onChange={(e) => set("quietEnd", e.currentTarget.value)} />
          </label>
        </div>
        <span className="hint">
          It stays quiet while you're gaming, watching something, away, or already chatting. Reminders you ask for
          always come through.
        </span>
      </fieldset>
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
