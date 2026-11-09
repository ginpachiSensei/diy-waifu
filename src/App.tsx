import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";
import { InstalledModel, OllamaStatus, PauseState, Settings, sameModel } from "./api";
import ChatTab from "./tabs/ChatTab";
import PersonaTab from "./tabs/PersonaTab";
import ModelTab from "./tabs/ModelTab";
import BodiesTab from "./tabs/BodiesTab";
import SystemTab from "./tabs/SystemTab";
import ActivityTab from "./tabs/ActivityTab";
import MemoryTab from "./tabs/MemoryTab";

const TABS = ["Chat", "Persona", "Memory", "Model", "Activity", "Bodies", "System"] as const;
type Tab = (typeof TABS)[number];

function App() {
  const [tab, setTab] = useState<Tab>("Chat");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [models, setModels] = useState<InstalledModel[]>([]);
  const [ollamaStatus, setOllamaStatus] = useState<OllamaStatus>("starting");
  const [pause, setPause] = useState<PauseState>({ paused: false, manual: false, autoReason: null });

  const refreshModels = useCallback(async () => {
    try {
      const list = await invoke<InstalledModel[]>("list_models");
      setModels(list);
      return list;
    } catch (e) {
      console.error(e);
      return [];
    }
  }, []);

  useEffect(() => {
    invoke<Settings>("get_settings").then(setSettings).catch(console.error);
    invoke<OllamaStatus>("get_ollama_status").then(setOllamaStatus).catch(console.error);
    invoke<PauseState>("get_pause_state").then(setPause).catch(console.error);

    const unlisten = [
      listen<OllamaStatus>("ollama:status", (e) => setOllamaStatus(e.payload)),
      listen<Settings>("settings:changed", (e) => setSettings(e.payload)),
      listen<PauseState>("companion:paused", (e) => setPause(e.payload)),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, []);

  useEffect(() => {
    if (ollamaStatus === "ready") refreshModels();
  }, [ollamaStatus, refreshModels]);

  async function toggleManualPause() {
    try {
      setPause(await invoke<PauseState>("set_paused", { paused: !pause.manual }));
    } catch (e) {
      console.error(e);
    }
  }

  const modelReady = settings != null && models.some((m) => sameModel(m.name, settings.model));

  return (
    <div className="app">
      <header>
        <h1>{settings?.companionName ?? "Companion"}</h1>
        <div className="pills">
          <span className={`pill ${ollamaStatus === "ready" ? "good" : ollamaStatus === "failed" ? "bad" : ""}`}>
            Ollama {ollamaStatus}
          </span>
          {settings && ollamaStatus === "ready" && (
            <button className={`pill ${modelReady ? "good" : "bad"}`} onClick={() => setTab("Model")}>
              {settings.model}
              {!modelReady && " (not downloaded)"}
            </button>
          )}
          {pause.manual && (
            <button className="pill warn" onClick={toggleManualPause}>
              paused · resume
            </button>
          )}
          {!pause.manual && pause.autoReason && (
            <button className="pill warn" onClick={() => setTab("System")} title="Auto-pause settings">
              auto-paused: {pause.autoReason}
            </button>
          )}
        </div>
      </header>

      <nav className="tabs">
        {TABS.map((t) => (
          <button key={t} className={t === tab ? "active" : ""} onClick={() => setTab(t)}>
            {t}
          </button>
        ))}
      </nav>

      {/* Tabs stay mounted so chat streams and downloads keep going when you switch */}
      <main>
        <div hidden={tab !== "Chat"} className="tab-chat">
          <ChatTab settings={settings} />
        </div>
        <div hidden={tab !== "Persona"}>
          <PersonaTab settings={settings} onSaved={setSettings} />
        </div>
        <div hidden={tab !== "Model"}>
          <ModelTab
            settings={settings}
            models={models}
            ollamaStatus={ollamaStatus}
            refreshModels={refreshModels}
            onSaved={setSettings}
          />
        </div>
        <div hidden={tab !== "Memory"}>
          <MemoryTab settings={settings} onSaved={setSettings} />
        </div>
        <div hidden={tab !== "Activity"}>
          <ActivityTab settings={settings} onSaved={setSettings} />
        </div>
        <div hidden={tab !== "Bodies"}>
          <BodiesTab />
        </div>
        <div hidden={tab !== "System"}>
          <SystemTab
            ollamaStatus={ollamaStatus}
            pause={pause}
            toggleManualPause={toggleManualPause}
            settings={settings}
            onSaved={setSettings}
          />
        </div>
      </main>
    </div>
  );
}

export default App;
