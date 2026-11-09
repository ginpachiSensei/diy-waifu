import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Activity, ActivityRule, Settings, errorText } from "../api";

interface Props {
  settings: Settings | null;
  onSaved: (s: Settings) => void;
}

const CATEGORIES = ["coding", "video", "gaming", "music", "browsing", "chatting", "creative"];

function minutes(ms: number) {
  const m = Math.round(ms / 60000);
  return m >= 60 ? `${Math.floor(m / 60)}h ${m % 60}m` : `${m}m`;
}

function localMidnight() {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

export default function ActivityTab({ settings, onSaved }: Props) {
  const [activity, setActivity] = useState<Activity | null>(null);
  const [totals, setTotals] = useState<[string, number][]>([]);
  const [rules, setRules] = useState<ActivityRule[]>([]);
  const [privacy, setPrivacy] = useState<ActivityRule[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  const [now, setNow] = useState(Date.now());

  useEffect(() => {
    invoke<Activity>("get_activity").then(setActivity).catch(console.error);
    const u = listen<Activity>("activity:changed", (e) => setActivity(e.payload));
    const loadTotals = () =>
      invoke<[string, number][]>("get_activity_totals", { since: localMidnight() })
        .then(setTotals)
        .catch(console.error);
    loadTotals();
    const t = window.setInterval(() => {
      loadTotals();
      setNow(Date.now());
    }, 60_000);
    return () => {
      u.then((f) => f());
      window.clearInterval(t);
    };
  }, []);

  useEffect(() => {
    if (!settings) return;
    setRules(settings.activityRules);
    setPrivacy(settings.privacyRules);
  }, [settings]);

  if (!settings) return <p className="muted">Loading…</p>;

  async function save(patch: Partial<Settings>, message = "Saved.") {
    if (!settings) return;
    try {
      onSaved(await invoke<Settings>("save_settings", { settings: { ...settings, ...patch } }));
      setStatus(message);
    } catch (e) {
      setStatus(errorText(e));
    }
  }

  const rulesDirty = JSON.stringify(rules) !== JSON.stringify(settings.activityRules);
  const privacyDirty = JSON.stringify(privacy) !== JSON.stringify(settings.privacyRules);
  const current = activity?.enabled && activity.app && activity.category !== "private" ? activity.app : null;
  const totalMs = totals.reduce((sum, [, ms]) => sum + ms, 0);

  return (
    <div className="stack">
      {status && <p className="muted">{status}</p>}

      <section>
        <h3>Right now</h3>
        {!activity?.enabled ? (
          <p className="muted">Screen awareness is off.</p>
        ) : !activity.app ? (
          <p className="muted">Waiting for the first check…</p>
        ) : (
          <>
            <p>
              <strong>{activity.label}</strong> · <span className="emotion">{activity.category}</span> ·{" "}
              {minutes(now - activity.since)} · {activity.presence}
            </p>
            {activity.title && <p className="hint">Window: {activity.title}</p>}
          </>
        )}
      </section>

      <section>
        <h3>Today</h3>
        {totals.length === 0 ? (
          <p className="muted">Nothing recorded yet today.</p>
        ) : (
          <ul className="list">
            {totals.map(([cat, ms]) => (
              <li key={cat}>
                <span className="grow">
                  {cat}
                  <span className="bar">
                    <span style={{ width: `${Math.max(2, (ms / totalMs) * 100)}%` }} />
                  </span>
                </span>
                <span className="muted">{minutes(ms)}</span>
              </li>
            ))}
          </ul>
        )}
        <p className="hint">Only app names, categories and times are saved, never window titles. Kept 30 days.</p>
      </section>

      <section>
        <h3>Privacy</h3>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.screenAwareness}
            onChange={(e) => save({ screenAwareness: e.currentTarget.checked })}
          />
          Screen awareness: notice which app I'm using
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.shareTitles}
            disabled={!settings.screenAwareness}
            onChange={(e) => save({ shareTitles: e.currentTarget.checked })}
          />
          Tell the companion the window title too (e.g. the file or video name), not just the app
        </label>
      </section>

      <section>
        <h3>Categories</h3>
        <p className="hint">
          Checked top to bottom; the first rule that matches wins. App is the exact process name; title and path
          match if they contain the text.
        </p>
        <RuleTable rules={rules} onChange={setRules} withCategory />
        <div className="row">
          <button onClick={() => setRules([...rules, { category: "", app: "", title: "", path: "" }])}>
            Add rule
          </button>
          {current && (
            <button
              onClick={() => setRules([{ category: "", app: current, title: "", path: "" }, ...rules])}
              title="Adds a rule for the app in front at the top of the list"
            >
              Add rule for {activity?.label}
            </button>
          )}
          <button className="primary" disabled={!rulesDirty} onClick={() => save({ activityRules: rules })}>
            Save
          </button>
          <button disabled={!rulesDirty} onClick={() => setRules(settings.activityRules)}>
            Undo
          </button>
        </div>
      </section>

      <section>
        <h3>Never look at</h3>
        <p className="hint">Matching windows show up only as "private": no app, no title, nothing saved.</p>
        <RuleTable rules={privacy} onChange={setPrivacy} />
        <div className="row">
          <button onClick={() => setPrivacy([...privacy, { category: "private", app: "", title: "", path: "" }])}>
            Add
          </button>
          <button className="primary" disabled={!privacyDirty} onClick={() => save({ privacyRules: privacy })}>
            Save
          </button>
          <button disabled={!privacyDirty} onClick={() => setPrivacy(settings.privacyRules)}>
            Undo
          </button>
        </div>
      </section>
    </div>
  );
}

function RuleTable({
  rules,
  onChange,
  withCategory = false,
}: {
  rules: ActivityRule[];
  onChange: (r: ActivityRule[]) => void;
  withCategory?: boolean;
}) {
  const set = (i: number, key: keyof ActivityRule, value: string) =>
    onChange(rules.map((r, j) => (j === i ? { ...r, [key]: value } : r)));
  const move = (i: number, by: number) => {
    const j = i + by;
    if (j < 0 || j >= rules.length) return;
    const next = [...rules];
    [next[i], next[j]] = [next[j], next[i]];
    onChange(next);
  };

  return (
    <div className="rules">
      <div className={`rule head ${withCategory ? "" : "no-cat"}`}>
        {withCategory && <span>Category</span>}
        <span>App</span>
        <span>Title contains</span>
        <span>Path contains</span>
        <span />
      </div>
      {withCategory && (
        <datalist id="categories">
          {CATEGORIES.map((c) => (
            <option key={c} value={c} />
          ))}
        </datalist>
      )}
      {rules.map((r, i) => (
        <div key={i} className={`rule ${withCategory ? "" : "no-cat"}`}>
          {withCategory && (
            <input list="categories" value={r.category} onChange={(e) => set(i, "category", e.currentTarget.value)} />
          )}
          <input value={r.app} placeholder="code.exe" onChange={(e) => set(i, "app", e.currentTarget.value)} />
          <input value={r.title} onChange={(e) => set(i, "title", e.currentTarget.value)} />
          <input value={r.path} onChange={(e) => set(i, "path", e.currentTarget.value)} />
          <span className="rule-actions">
            {withCategory && (
              <>
                <button title="Move up" onClick={() => move(i, -1)} disabled={i === 0}>
                  ↑
                </button>
                <button title="Move down" onClick={() => move(i, 1)} disabled={i === rules.length - 1}>
                  ↓
                </button>
              </>
            )}
            <button title="Remove" onClick={() => onChange(rules.filter((_, j) => j !== i))}>
              ✕
            </button>
          </span>
        </div>
      ))}
    </div>
  );
}
