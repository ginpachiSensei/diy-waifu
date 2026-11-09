// Speaking replies with the Windows voices that WebView2 exposes through the
// Web Speech API. Used by the pet (for replies) and the Voice tab (for testing).

export interface SpeechSettings {
  ttsVoice: string;
  ttsRate: number;
  ttsPitch: number;
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

export function stopSpeaking() {
  if (canSpeak) speechSynthesis.cancel();
}
