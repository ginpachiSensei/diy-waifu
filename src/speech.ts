// Speaking replies, with the Windows voices that WebView2 exposes through the
// Web Speech API, Piper (tts.rs), Kokoro (kokoro.rs) or the heavy studio
// voices (studio.rs, Qwen3-TTS), the last three offline and synthesized in Rust. Used by the pet (for replies) and the Voice tab (for testing).
//
// Replies are spoken sentence by sentence while they stream in (ReplySpeaker),
// the way Open-LLM-VTuber and AIRI do it, so speech starts after the first
// sentence instead of after the whole reply.

import { invoke } from "@tauri-apps/api/core";

export interface SpeechSettings {
  ttsVoice: string;
  ttsRate: number;
  ttsPitch: number;
  ttsEngine?: string;
  speakReplies?: boolean;
}

export const canSpeak = typeof window !== "undefined" && "speechSynthesis" in window;

/** Voices load asynchronously the first time; this waits for them. */
export function loadVoices(): Promise<SpeechSynthesisVoice[]> {
  if (!canSpeak) return Promise.resolve([]);
  const now = speechSynthesis.getVoices();
  if (now.length) return Promise.resolve(now);
  return new Promise((resolve) => {
    const done = () => resolve(speechSynthesis.getVoices());
    speechSynthesis.addEventListener("voiceschanged", done, { once: true });
    setTimeout(done, 1500);
  });
}

/** Strips things that sound bad read aloud (emoji, markdown symbols). */
function speakable(text: string) {
  return text
    .replace(/\p{Extended_Pictographic}/gu, "")
    .replace(/[*_`#>~]/g, "")
    .trim();
}

/** Has kana or kanji: Piper and Kokoro can't say it, and Windows needs a Japanese voice. */
export const isJapanese = (text: string) => /[぀-ヿ一-鿿]/.test(text);

// Whatever is playing right now; stopSpeaking() ends it early
let current: { stop: () => void } | null = null;

/** Says `text` with a Windows voice; resolves when it has been said (or stopped). */
async function utter(text: string, s: SpeechSettings): Promise<void> {
  if (!canSpeak) return;
  const u = new SpeechSynthesisUtterance(text);
  const voices = await loadVoices();
  let voice = voices.find((v) => v.name === s.ttsVoice);
  // The chosen voice would read Japanese as gibberish; use an installed Japanese one
  if (isJapanese(text) && !voice?.lang.startsWith("ja")) {
    voice = voices.find((v) => v.lang.startsWith("ja")) ?? voice;
    if (voice) u.lang = voice.lang;
  }
  if (voice) u.voice = voice;
  u.rate = s.ttsRate;
  u.pitch = s.ttsPitch;
  return new Promise((resolve) => {
    // onend sometimes never fires in Chromium; don't let the queue hang on it
    const guard = window.setTimeout(done, 5000 + text.length * 150);
    function done() {
      window.clearTimeout(guard);
      if (current === entry) current = null;
      resolve();
    }
    const entry = { stop: () => (speechSynthesis.cancel(), done()) };
    current = entry;
    u.onend = u.onerror = done;
    speechSynthesis.speak(u);
  });
}

/** Plays a WAV; resolves when it ends (or is stopped). */
function playWav(wav: ArrayBuffer): Promise<void> {
  const url = URL.createObjectURL(new Blob([wav], { type: "audio/wav" }));
  const audio = new Audio(url);
  return new Promise((resolve) => {
    function done() {
      URL.revokeObjectURL(url);
      if (current === entry) current = null;
      resolve();
    }
    const entry = { stop: () => (audio.pause(), done()) };
    current = entry;
    audio.onended = audio.onerror = done;
    audio.play().catch(done);
  });
}

/** Says one line with a Windows voice, cutting off anything already playing. */
export async function speak(text: string, s: SpeechSettings, events: { onStart?: () => void; onEnd?: () => void } = {}) {
  const clean = speakable(text);
  if (!clean || !canSpeak) return;
  stopSpeaking();
  events.onStart?.();
  await utter(clean, s);
  events.onEnd?.();
}

/** The Tauri command that renders speech for an offline engine, or null for Windows voices. */
function synthCommand(engine?: string) {
  return ({ piper: "piper_speak", kokoro: "kokoro_speak", studio: "studio_speak" } as Record<string, string>)[engine ?? ""] ?? null;
}

/** Plays `text` with Piper or Kokoro (`voice` overrides the saved voice). Rejects with the reason if it can't. */
export async function speakOffline(engine: "piper" | "kokoro" | "studio", text: string, voice?: string) {
  const clean = speakable(text);
  if (!clean) return;
  const wav = await invoke<ArrayBuffer>(synthCommand(engine)!, { text: clean, voice });
  stopSpeaking();
  await playWav(wav);
}

export function stopSpeaking() {
  if (canSpeak) speechSynthesis.cancel();
  current?.stop();
  current = null;
}

// A sentence ends at . ! ? … followed by a space (so "3.5" and "v2.1" stay whole),
// at CJK / Devanagari full stops, or at a line break.
const BOUNDARY = /[.!?…]+["'”’)\]]*\s+|[。！？।]+\s*|\n+/g;
// Very short sentences ("Oh!") are joined to the next one, so Piper isn't
// started for two words and the voice doesn't sound choppy
const MIN_CHUNK = 24;

/**
 * Cuts finished sentences off the front of `text`. Returns them and how many
 * characters they used. With `final`, whatever is left counts as a sentence too.
 */
export function splitSentences(text: string, final: boolean): { chunks: string[]; used: number } {
  const chunks: string[] = [];
  let cut = 0;
  for (const m of text.matchAll(BOUNDARY)) {
    const end = m.index! + m[0].length;
    const piece = text.slice(cut, end).trim();
    if (piece.length >= MIN_CHUNK) {
      chunks.push(piece);
      cut = end;
    }
  }
  if (final) {
    const rest = text.slice(cut).trim();
    if (rest) chunks.push(rest);
    cut = text.length;
  }
  return { chunks, used: cut };
}

/**
 * Speaks a reply while it streams in. Feed it the body messages:
 * `reset()` on user/thinking, `restart()` on tool, `token()` and `finish()`.
 * Offline engines render the next sentence while the current one plays.
 */
export class ReplySpeaker {
  private streamed = "";
  private used = 0;
  private gen = 0;
  private pending = 0;
  private muted = false;
  // A reply is streaming in (between its first token and finish())
  private writing = false;
  private synth: Promise<unknown> = Promise.resolve();
  private play: Promise<unknown> = Promise.resolve();

  private settings: () => SpeechSettings | null;
  private onSpeaking: (on: boolean) => void;

  constructor(settings: () => SpeechSettings | null, onSpeaking: (on: boolean) => void = () => {}) {
    this.settings = settings;
    this.onSpeaking = onSpeaking;
  }

  /** A new turn: stop talking and forget the old reply. */
  reset() {
    this.muted = false;
    this.writing = false;
    this.stop();
  }

  /** You cut in: stop now, and stay quiet for the rest of the reply being written, if any. */
  interrupt() {
    this.muted = this.writing;
    this.stop();
  }

  /** A lookup started; text streamed before it is thrown away by the bodies. */
  restart() {
    this.streamed = "";
    this.used = 0;
  }

  token(text: string) {
    this.writing = true;
    if (this.muted) return;
    this.streamed += text;
    const { chunks, used } = splitSentences(this.streamed.slice(this.used), false);
    this.used += used;
    chunks.forEach((c) => this.say(c));
  }

  /** The final reply: speak whatever the stream hasn't covered yet. */
  finish(text: string) {
    this.writing = false;
    if (this.muted) {
      this.muted = false;
      return;
    }
    const spoken = this.streamed.slice(0, this.used).trimStart();
    const full = text.trimStart();
    let rest = "";
    if (!spoken) rest = full;
    else if (full.startsWith(spoken)) rest = full.slice(spoken.length);
    // else: the final text differs from what was streamed and spoken; don't repeat it
    splitSentences(rest, true).chunks.forEach((c) => this.say(c));
    this.streamed = "";
    this.used = 0;
  }

  private stop() {
    this.gen++;
    this.streamed = "";
    this.used = 0;
    this.synth = Promise.resolve();
    this.play = Promise.resolve();
    stopSpeaking();
    if (this.pending) {
      this.pending = 0;
      this.onSpeaking(false);
    }
  }

  private say(sentence: string) {
    const s = this.settings();
    const clean = speakable(sentence);
    if (this.muted || !s?.speakReplies || !clean) return;
    const gen = this.gen;
    if (this.pending++ === 0) this.onSpeaking(true);

    // Offline engines render one sentence at a time, ahead of playback.
    // Piper and Kokoro only speak English (and Hindi), so Japanese goes to Windows.
    let audio: Promise<ArrayBuffer | null> = Promise.resolve(null);
    const english = s.ttsEngine === "piper" || s.ttsEngine === "kokoro";
    const command = english && isJapanese(clean) ? null : synthCommand(s.ttsEngine);
    if (command) {
      audio = this.synth.then(() => (gen === this.gen ? invoke<ArrayBuffer>(command, { text: clean }) : null));
      this.synth = audio.catch(() => null);
    }
    this.play = this.play.then(async () => {
      if (gen !== this.gen) return;
      let wav: ArrayBuffer | null = null;
      try {
        wav = await audio;
      } catch (e) {
        console.warn(`${s.ttsEngine} failed, using a Windows voice`, e);
      }
      if (gen !== this.gen) return;
      if (wav) await playWav(wav);
      else await utter(clean, s);
      if (gen === this.gen && --this.pending === 0) this.onSpeaking(false);
    });
  }
}
