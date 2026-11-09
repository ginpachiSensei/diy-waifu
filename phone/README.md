# Phone body

Use a spare Android phone as the companion's screen instead of, or alongside, the ESP32.
It's a plain HTML/JS page with no build step. The Tauri app compiles these files in (see `src-tauri/src/phone.rs`) and serves them on port **8081**. The page connects to the same WebSocket on port **8080** that the ESP32 uses, with the same wire format (`{"emotion", "text"}`).

## Use it

1. Run the app (`npm run tauri:dev`). The **Phone Body** panel shows a URL like `http://192.168.1.20:8081/`.
2. Open that URL in Chrome on the phone. The phone must be on the same Wi-Fi as the PC.
3. Tap ⛶ for fullscreen. In fullscreen the bottom bar hides until you tap the screen.

The first time, Windows Firewall may ask to allow `diy-waifu`. Allow it on **private networks**. If it never asked, open inbound TCP 8080 and 8081.

## Features

- Shows the emotion GIF, the emotion name and the reply. Landscape puts the GIF on the left like the ESP32; portrait stacks the GIF above the text.
- Type a message and it's sent as `{"text": ...}`, the same as the ESP32 would send.
- 🔊 reads replies aloud with the phone's text-to-speech.
- 🎤 hold to talk, with the screen kept awake. Both of these only work in a **secure context** (see below).
- Reconnects on its own if the app restarts.
- `?ws=ws://host:port` on the URL overrides the WebSocket address.

## Mic and keep-awake (secure context)

Chrome only enables speech recognition and the Wake Lock API on `https://` or `localhost`, so they're unavailable on a plain `http://192.168...` page. The page hides the 🎤 button when the mic can't be used. Options:

- **USB (simplest):** enable USB debugging on the phone, then on the PC run
  `adb reverse tcp:8080 tcp:8080 && adb reverse tcp:8081 tcp:8081`
  and open `http://localhost:8081/` on the phone.
- **Wi-Fi:** in Chrome on the phone, open `chrome://flags/#unsafely-treat-insecure-origin-as-secure`, add `http://<pc-ip>:8081`, and restart Chrome.
- Without either, set the phone's screen timeout to the maximum, or turn on "Stay awake while charging" in Developer options.

## Adding GIFs

1. Put the file in `gifs/` at the repo root.
2. Add a row to `FILES` in `src-tauri/src/phone.rs`.
3. Point an emotion at it in `EMOTION_GIFS` in `app.js`.

Unlike the ESP32, the phone doesn't need resizing or header conversion.

Changes to these files need a Rust rebuild to show up, because they're compiled into the app. `tauri dev` rebuilds on its own.
