// Types for avatar.js, so the desktop pet (TypeScript) can import it.

export interface GifManifest {
  default?: string;
  states?: Record<string, string>;
}

export interface CompanionMessage {
  type?: string;
  text?: string;
  emotion?: string;
  state?: string;
  source?: string;
  label?: string;
  sources?: { title: string; url: string }[];
}

export interface Avatar {
  readonly state: string;
  thinking(): void;
  searching(): void;
  token(): void;
  reply(emotion?: string): void;
  setState(state: string | null): void;
  handle(msg: CompanionMessage): void;
}

export const REPLY_HOLD_MS: number;
export function gifFor(manifest: GifManifest | null, state: string): string | null;
export function createAvatar(onChange: (state: string) => void): Avatar;
