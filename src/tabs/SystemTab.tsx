import { OllamaStatus } from "../api";

interface Props {
  ollamaStatus: OllamaStatus;
  paused: boolean;
  togglePause: () => void;
}

export default function SystemTab({ ollamaStatus, paused, togglePause }: Props) {
  return (
    <div className="stack">
      <section>
        <h3>Pause</h3>
        <p>Pausing frees the model's memory and turns away new messages until you resume.</p>
        <button className={paused ? "primary" : ""} onClick={togglePause}>
          {paused ? "Resume companion" : "Pause companion"}
        </button>
      </section>

      <section>
        <h3>Ollama</h3>
        <p>
          {ollamaStatus === "ready" && <span className="ok">running</span>}
          {ollamaStatus === "starting" && <span className="muted">starting…</span>}
          {ollamaStatus === "failed" && <span className="error">failed to start</span>}
        </p>
        <p className="hint">
          Logs are in <code>%LOCALAPPDATA%\com.satyam.diy-waifu\logs</code>, and chat history in{" "}
          <code>%APPDATA%\com.satyam.diy-waifu\companion.db</code>.
        </p>
      </section>
    </div>
  );
}
