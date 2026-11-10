// Speaking replies, with either the Windows voices that WebView2 exposes through
// the Web Speech API, or Piper (natural offline voices, synthesized in Rust; see
// tts.rs). Used by the pet (for replies) and the Voice tab (for testing).

import { invoke } from "@tauri-apps/api/core";

export interface SpeechSettings {
  ttsVoice: string;
  ttsRate: number;
  ttsPitch: number;
  ttsEngine?: string;
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

export async function speak(text: string, s: SpeechSettings, events: { onStart?: () => void; onEnd?: () => void } = {}) {
  if (!canSpeak) return;
  const clean = speakable(text);
  if (!clean) return;
  speechSynthesis.cancel();
  const u = new SpeechSynthesisUtterance(clean);
  const voices = await loadVoices();
  const voice = voices.find((v) => v.name === s.ttsVoice);
  if (voice) u.voice = voice;
  u.rate = s.ttsRate;
  u.pitch = s.ttsPitch;
  u.onstart = () => events.onStart?.();
  u.onend = () => events.onEnd?.();
  u.onerror = () => events.onEnd?.();
  speechSynthesis.speak(u);
}

let piperAudio: HTMLAudioElement | null = null;

/** Plays `text` with Piper (`voice` overrides the saved voice). Rejects with the reason if it can't. */
export async function speakPiper(text: string, voice?: string) {
  const clean = speakable(text);
  if (!clean) return;
  const wav = await invoke<ArrayBuffer>("piper_speak", { text: clean, voice });
  stopSpeaking();
  const url = URL.createObjectURL(new Blob([wav], { type: "audio/wav" }));
  const audio = new Audio(url);
  piperAudio = audio;
  audio.onended = audio.onerror = () => URL.revokeObjectURL(url);
  await audio.play();
}

/** Speaks a reply with the engine from settings, falling back to Windows voices. */
export async function speakReply(text: string, s: SpeechSettings) {
  if (s.ttsEngine === "piper") {
    try {
      await speakPiper(text);
      return;
    } catch (e) {
      console.warn("Piper failed, using a Windows voice", e);
    }
  }
  await speak(text, s);
}

export function stopSpeaking() {
  if (canSpeak) speechSynthesis.cancel();
  if (piperAudio) {
    piperAudio.pause();
    piperAudio = null;
  }
}
