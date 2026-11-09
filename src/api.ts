// Types and helpers shared by the control panel tabs. These mirror the Rust
// structs in src-tauri/src (db.rs, settings.rs, ollama.rs).

export interface ChatMessage {
  id: number;
  ts: number; // unix millis
  role: "user" | "assistant";
  text: string;
  emotion: string | null;
  source: string; // panel | phone | esp32 | pet | companion | body
}

export type ReplyLength = "short" | "medium" | "long";

export interface Settings {
  model: string;
  companionName: string;
  userName: string;
  personality: string;
  replyLength: ReplyLength;
}

export interface InstalledModel {
  name: string;
  size: number; // bytes
}

export interface PullProgress {
  model: string;
  status: string;
  completed: number | null;
  total: number | null;
}

export type OllamaStatus = "starting" | "ready" | "failed";

export const RECOMMENDED_MODELS: { name: string; size: string; note: string }[] = [
  { name: "qwen2.5:1.5b", size: "~1 GB", note: "Fastest. Forgets the thread in longer chats." },
  { name: "qwen2.5:3b", size: "~1.9 GB", note: "Default. Good balance on most PCs." },
  { name: "llama3.2:3b", size: "~2 GB", note: "Similar size, chattier tone." },
  { name: "gemma3:4b", size: "~3.3 GB", note: "Better replies, needs ~6 GB free RAM." },
  { name: "qwen2.5:7b", size: "~4.7 GB", note: "Best quality here. Slow without a GPU." },
];

/** Ollama lists untagged models as `name:latest`. */
export function sameModel(a: string, b: string): boolean {
  const norm = (s: string) => (s.includes(":") ? s : `${s}:latest`);
  return norm(a) === norm(b);
}

export function formatBytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${Math.round(n / 1e6)} MB`;
  return `${Math.round(n / 1e3)} KB`;
}

export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}
