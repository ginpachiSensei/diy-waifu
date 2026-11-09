// Phone "body" for the companion app. Does the same job as esp32/src/main.cpp:
// connects to the app's WebSocket and shows each reply next to the emotion's GIF
// (it streams in as "token" messages, then a final "reply" with the emotion). Also lets you type or speak a message, and can
// read replies aloud.
//
// The Tauri app serves this page on port 8081. The WebSocket is on 8080 of the
// same host. Add ?ws=ws://host:port to the URL to point it somewhere else.

(() => {
  "use strict";

  // ---- Emotion -> GIF table (mirrors EMOTION_GIFS in the ESP32 firmware) ----
  // Every emotion uses the wave GIF for now. Add a file to gifs/ and to the
  // FILES table in src-tauri/src/phone.rs, then point a row here at it.
  const EMOTION_GIFS = {
    neutral: "gifs/mai-san-wave.gif",
    happy: "gifs/mai-san-wave.gif",
    excited: "gifs/mai-san-wave.gif",
    sad: "gifs/mai-san-wave.gif",
    thinking: "gifs/mai-san-wave.gif",
  };
  const gifForEmotion = (e) => EMOTION_GIFS[e] || EMOTION_GIFS.neutral;

  const RECONNECT_MS = 3000;
  const REPLY_TIMEOUT_MS = 90000; // give up waiting if the app goes quiet mid-reply

  const params = new URLSearchParams(location.search);
  const WS_URL = params.get("ws") || `ws://${location.hostname || "localhost"}:8080`;

  const $ = (id) => document.getElementById(id);
  const gifEl = $("gif");
  const emotionEl = $("emotion");
  const textEl = $("text");
  const statusEl = $("status");
  const form = $("bar");
  const input = $("input");
  const micBtn = $("mic");
  const speakBtn = $("speak");
  const fsBtn = $("fullscreen");

  // ---- Small storage helpers (storage can be blocked) ----
  const load = (k, d) => { try { return localStorage.getItem(k) ?? d; } catch { return d; } };
  const save = (k, v) => { try { localStorage.setItem(k, v); } catch { /* ignore */ } };

  // ---- Display ----
  function setGif(emotion) {
    const src = gifForEmotion(emotion);
    // Only swap when it changes, otherwise the animation restarts
    if (gifEl.getAttribute("src") !== src) gifEl.setAttribute("src", src);
  }

  function showReply(emotion, text) {
    streaming = false;
    emotionEl.textContent = emotion;
    textEl.textContent = text;
    textEl.classList.remove("thinking");
    setGif(emotion);
  }

  let replyTimer = null;
  function showThinking(userText) {
    streaming = false;
    emotionEl.textContent = "thinking";
    textEl.textContent = `“${userText}”`;
    textEl.classList.add("thinking");
    setGif("thinking");
    armReplyTimeout();
  }

  function armReplyTimeout() {
    clearTimeout(replyTimer);
    replyTimer = setTimeout(() => {
      showReply("neutral", "No reply from the companion app.");
    }, REPLY_TIMEOUT_MS);
  }

  // The first token replaces the quoted message; the rest add on
  let streaming = false;
  function appendToken(token) {
    if (!streaming) {
      streaming = true;
      textEl.textContent = "";
      textEl.classList.remove("thinking");
    }
    textEl.textContent += token;
    armReplyTimeout();
  }

  function showStatus(label, cls) {
    statusEl.textContent = label;
    statusEl.className = cls;
  }

  // ---- WebSocket ----
  let ws = null;

  function connect() {
    showStatus("connecting", "connecting");
    try {
      ws = new WebSocket(WS_URL);
    } catch (e) {
      console.error(e);
      setTimeout(connect, RECONNECT_MS);
      return;
    }

    ws.onopen = () => {
      showStatus("linked", "online");
      ws.send(JSON.stringify({ type: "hello", body: "phone" }));
    };

    ws.onmessage = (ev) => {
      let msg;
      try { msg = JSON.parse(ev.data); } catch { return; }
      if (!msg || typeof msg !== "object") return;
      const text = typeof msg.text === "string" ? msg.text : "";
      // Messages without a type come from older app versions and are replies
      switch (msg.type || "reply") {
        case "user": // someone (here or on another body) said something
          showThinking(text);
          break;
        case "token": // a streamed piece of the reply
          appendToken(text);
          break;
        case "reply": {
          clearTimeout(replyTimer);
          const emotion = typeof msg.emotion === "string" ? msg.emotion.toLowerCase() : "neutral";
          showReply(emotion, text);
          speak(text);
          break;
        }
        default: // "thinking", "state", and anything newer: nothing to show yet
          break;
      }
    };

    ws.onclose = () => {
      showStatus("offline", "offline");
      setTimeout(connect, RECONNECT_MS);
    };
    ws.onerror = () => ws.close();
  }

  function send(text) {
    text = text.trim();
    if (!text) return false;
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      showReply("sad", "Not connected to the companion app.");
      return false;
    }
    ws.send(JSON.stringify({ text }));
    showThinking(text);
    return true;
  }

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (send(input.value)) input.value = "";
  });

  // ---- Text to speech ----
  let speakOn = load("speak", "0") === "1";
  const canSpeak = "speechSynthesis" in window;

  function renderSpeakBtn() {
    speakBtn.textContent = speakOn ? "🔊" : "🔇";
  }

  function speak(text) {
    if (!speakOn || !canSpeak || !text) return;
    speechSynthesis.cancel();
    const u = new SpeechSynthesisUtterance(text);
    u.pitch = 1.3;
    speechSynthesis.speak(u);
  }

  if (!canSpeak) speakBtn.hidden = true;
  renderSpeakBtn();
  speakBtn.addEventListener("click", () => {
    speakOn = !speakOn;
    save("speak", speakOn ? "1" : "0");
    renderSpeakBtn();
    if (!speakOn && canSpeak) speechSynthesis.cancel();
  });

  // ---- Speech to text (needs a secure context: https or localhost) ----
  const Recognition = window.SpeechRecognition || window.webkitSpeechRecognition;
  if (Recognition && window.isSecureContext) {
    micBtn.hidden = false;
    let rec = null;

    const start = (e) => {
      e.preventDefault();
      if (rec) return;
      rec = new Recognition();
      rec.lang = navigator.language || "en-US";
      rec.interimResults = true;
      rec.onresult = (ev) => {
        input.value = Array.from(ev.results).map((r) => r[0].transcript).join("");
      };
      rec.onend = () => {
        micBtn.classList.remove("listening");
        rec = null;
        if (send(input.value)) input.value = "";
      };
      rec.onerror = (ev) => console.warn("speech error", ev.error);
      micBtn.classList.add("listening");
      rec.start();
    };
    const stop = () => rec && rec.stop();

    // Hold to talk
    micBtn.addEventListener("pointerdown", start);
    micBtn.addEventListener("pointerup", stop);
    micBtn.addEventListener("pointerleave", stop);
    micBtn.addEventListener("pointercancel", stop);
  }

  // ---- Keep the screen on (Wake Lock also needs a secure context) ----
  let wakeLock = null;
  async function keepAwake() {
    if (!("wakeLock" in navigator) || wakeLock) return;
    try {
      wakeLock = await navigator.wakeLock.request("screen");
      wakeLock.addEventListener("release", () => { wakeLock = null; });
    } catch (e) {
      console.warn("wake lock failed", e);
    }
  }
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") keepAwake();
  });
  document.addEventListener("pointerdown", keepAwake, { once: true });

  // ---- Fullscreen "kiosk" mode: hides the bar until you tap the screen ----
  let barTimer = null;
  fsBtn.addEventListener("click", async () => {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else await document.documentElement.requestFullscreen({ navigationUI: "hide" });
    } catch (e) {
      console.warn("fullscreen failed", e);
    }
    keepAwake();
  });
  document.addEventListener("fullscreenchange", () => {
    document.body.classList.toggle("kiosk", !!document.fullscreenElement);
  });
  $("body").addEventListener("click", () => {
    if (!document.body.classList.contains("kiosk")) return;
    document.body.classList.add("show-bar");
    clearTimeout(barTimer);
    barTimer = setTimeout(() => {
      if (document.activeElement !== input) document.body.classList.remove("show-bar");
    }, 5000);
  });

  // ---- Boot ----
  showReply("hello", "Waiting for the companion app...");
  connect();
})();
