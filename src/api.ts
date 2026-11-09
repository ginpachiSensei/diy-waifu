// Types and helpers shared by the control panel tabs. These mirror the Rust
// structs in src-tauri/src (db.rs, settings.rs, ollama.rs).

export interface ChatMessage {
  id: number;
  ts: number; // unix millis
  role: "user" | "assistant";
  text: string;
  emotion: string | null;
  source: string; // panel | phone | esp32 | pet | companion | body
  sources: Source[]; // web pages a reply was based on
}

export interface Source {
  title: string;
  url: string;
}

export type ReplyLength = "short" | "medium" | "long";

export interface Settings {
  model: string;
  companionName: string;
  userName: string;
  personality: string;
  replyLength: ReplyLength;
  autoPause: boolean;
  heavyApps: string[];
  petVisible: boolean;
  screenAwareness: boolean;
  shareTitles: boolean;
  activityRules: ActivityRule[];
  privacyRules: ActivityRule[];
  internetAccess: boolean;
  braveApiKey: string;
  askBeforeLookup: boolean;
  memoryLearning: boolean;
  proactive: boolean;
  proactiveGapMinutes: number;
  breakAfterMinutes: number;
  quietStart: string; // "HH:MM"
  quietEnd: string;
  voiceInput: boolean;
  whisperModel: string;
  pttShortcut: string;
  speakReplies: boolean;
  ttsVoice: string;
  ttsRate: number;
  ttsPitch: number;
}

export interface VoiceModel {
  id: string;
  sizeMb: number;
  note: string;
  downloaded: boolean;
}

export interface VoiceStatus {
  models: VoiceModel[];
  microphone: string | null;
  recording: boolean;
}

export interface VoiceState {
  state: "idle" | "listening" | "transcribing" | "error";
  error: string | null;
}

export interface VoiceProgress {
  model: string;
  completed: number;
  total: number | null;
  done?: boolean;
}

export interface Fact {
  id: number;
  text: string;
  origin: "auto" | "user";
  created: number;
  lastUsed: number;
}

export interface Summary {
  id: number;
  ts: number;
  fromId: number;
  toId: number;
  text: string;
}

export interface Reminder {
  id: number;
  dueTs: number;
  text: string;
  created: number;
  done: boolean;
}

export interface MemoryStatus {
  embedModel: string;
  embedReady: boolean;
  facts: number;
  summaries: number;
  messages: number;
  embeddedMessages: number;
}

/** Empty fields match anything; app is an exact process name, title/path are substrings. */
export interface ActivityRule {
  category: string;
  app: string;
  title: string;
  path: string;
}

export interface Activity {
  enabled: boolean;
  app: string | null;
  label: string | null;
  category: string | null;
  title: string | null;
  presence: "active" | "idle" | "away";
  since: number;
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

export interface PauseState {
  paused: boolean;
  manual: boolean;
  autoReason: string | null; // e.g. "blender.exe is running"
}

export interface DesktopState {
  petVisible: boolean;
  clickThrough: boolean;
  autostart: boolean;
}

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

export interface Pairing {
  code: string;
  lanIp: string | null;
  phoneUrl: string | null;
  qrSvg: string | null;
}
