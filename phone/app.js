// Phone "body" for the companion app. Does the same job as esp32/src/main.cpp:
// connects to the app's WebSocket and shows each reply as it streams in ("token"
// messages, then a final "reply" with the emotion). The GIF comes from the
// shared avatar state machine (avatar.js) and gifs/manifest.json. Also lets you
// type or speak a message, and can read replies aloud.
//
// The Tauri app serves this page on port 8081. The WebSocket is on 8080 of the
// same host. Add ?ws=ws://host:port to the URL to point it somewhere else.
// ?token=CODE is the pairing code from the app's Bodies tab (the QR code there
// includes it). It's remembered, so the plain URL works afterwards.

import { createAvatar, gifFor } from "./avatar.js";

(() => {
  // ---- Avatar: which GIF to show. Add art by editing gifs/manifest.json ----
  let manifest = { default: "mai-san-wave.gif", states: {} };
  const avatar = createAvatar((state) => setGif(state));
  fetch("gifs/manifest.json")
    .then((r) => r.json())
    .then((m) => {
      manifest = m;
      setGif(avatar.state);
    })
    .catch((e) => console.warn("couldn't load gif manifest", e));

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

  // ---- Pairing code ----
  if (params.get("token")) {
    save("pairToken", params.get("token").trim());
    // Keep the code out of the address bar (and screenshots of it)
    params.delete("token");
    const rest = params.toString();
    history.replaceState(null, "", location.pathname + (rest ? `?${rest}` : ""));
  }
  function wsUrl() {
    const token = load("pairToken", "");
    if (!token) return WS_URL;
    const u = new URL(WS_URL);
    u.searchParams.set("token", token);
    return u.toString();
  }

  // ---- Display ----
  function setGif(state) {
    const name = gifFor(manifest, state);
    if (!name) return;
    const src = `gifs/${name}`;
    // Only swap when it changes, otherwise the animation restarts
    if (gifEl.getAttribute("src") !== src) gifEl.setAttribute("src", src);
  }

  function showReply(emotion, text) {
    streaming = false;
    emotionEl.textContent = emotion;
    textEl.textContent = text;
    textEl.classList.remove("thinking");
    avatar.reply(emotion);
  }

  let replyTimer = null;
  function showThinking(userText) {
    streaming = false;
    emotionEl.textContent = "thinking";
    textEl.textContent = `“${userText}”`;
    textEl.classList.add("thinking");
    avatar.thinking();
    armReplyTimeout();
  }

  function showLookup(label) {
    streaming = false; // text streamed before the lookup is dropped
    emotionEl.textContent = "searching";
    textEl.textContent = `🔎 ${label}…`;
    textEl.classList.add("thinking");
    avatar.searching();
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
    avatar.token();
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
      ws = new WebSocket(wsUrl());
    } catch (e) {
      console.error(e);
      setTimeout(connect, RECONNECT_MS);
      return;
    }

    let opened = false;
    ws.onopen = () => {
      opened = true;
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
        case "tool": // looking something up, e.g. "searching: weather in Delhi"
          showLookup(msg.label || "looking it up");
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
        case "state": // e.g. "away" or an activity like "coding"
          avatar.setState(msg.state);
          break;
        default: // "thinking" (already shown on "user") and anything newer
          break;
      }
    };

    ws.onclose = () => {
      // Refused during the handshake: usually a missing or old pairing code
      if (!opened && load("pairToken", "") === "" && !["localhost", "127.0.0.1"].includes(location.hostname)) {
        showStatus("scan the QR code in the app", "offline");
      } else {
        showStatus(opened ? "offline" : "offline (pairing code changed?)", "offline");
      }
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
