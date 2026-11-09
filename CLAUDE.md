# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A desktop "AI companion brain" built with Tauri 2 (Rust backend) + React 19 / Vite / TypeScript frontend. It runs a local LLM through a bundled Ollama sidecar and serves replies over a WebSocket to a "body" that shows text and an emotion. There are two bodies, and both can be connected at once: an ESP32 with an ILI9341 display (`esp32/`), and an Android phone running a web page (`phone/`).

## Commands

```sh
npm install
npm run tauri:dev      # downloads Ollama sidecar if missing, then runs the full app (Vite on :1420 + Rust)
npm run tauri:build    # same sidecar step, then a release bundle
npm run setup:ollama   # just fetch the Ollama binary into src-tauri/binaries/
npm run dev            # frontend only in a browser (Tauri invoke/listen calls will fail)
npm run build          # tsc type-check + vite build (the only frontend "lint")
```

Rust-only checks, run from `src-tauri/`: `cargo check`, `cargo clippy`, `cargo test` (single test: `cargo test parses_emotion_tag_variants`). `cargo test live_chat_streams -- --ignored --nocapture` runs a real chat against a running Ollama. If the dev app is open, the build can't overwrite `target/debug/ollama.exe`; use `CARGO_TARGET_DIR=target/lint` for checks.

The Ollama sidecar lives in `src-tauri/binaries/` (gitignored) and has two parts that must ship together. `ollama-<target-triple>[.exe]` is bundled through `bundle.externalBin`. `lib/ollama/` holds the model runner (`llama-server` plus ggml DLLs) and is bundled through `bundle.resources`. Ollama looks for `lib/ollama` next to its own exe, and Tauri copies resources into `target/debug` in dev. If the runner is missing, Ollama still starts but every `/api/chat` returns 500. `setup-ollama.cjs` downloads both and copies only the CPU runner, skipping the CUDA, ROCm and Vulkan folders.

## Architecture

Everything starts in `run()` in [src-tauri/src/lib.rs](src-tauri/src/lib.rs). The setup hook spawns four long-running async tasks:

1. **WebSocket server** ([websocket.rs](src-tauri/src/websocket.rs)) on `0.0.0.0:8080`. Each client gets a `tokio::select!` loop that:
   - forwards every message from the shared `broadcast::Sender<String>` to the client, and
   - parses incoming JSON. `{"type":"hello","body":"phone|esp32|pet"}` names the client, and `{"text"}` is passed to `companion::handle_user_message` in a spawned task, with the body name as the source.
2. **Ollama sidecar** ([ollama.rs](src-tauri/src/ollama.rs)): runs `ollama serve` via `tauri-plugin-shell`, polls `localhost:11434/api/tags` until it's ready, and stores the child in `AppState.ollama_child` so it's killed on `RunEvent::ExitRequested`.
3. **Phone page server** ([phone.rs](src-tauri/src/phone.rs)) on `0.0.0.0:8081`. This is a minimal HTTP server for the `phone/` files and `gifs/`, compiled in with `include_bytes!`. Only paths in its `FILES` table exist.
4. **Process monitor** ([monitor.rs](src-tauri/src/monitor.rs)): every 5s checks for heavy apps (Blender, Unity) and emits `companion:paused`.

Shared state is `AppState { broadcast_tx, ollama_child, ollama_status, paused, chat_lock }`, plus a managed `db::Db`.

**The brain** ([companion.rs](src-tauri/src/companion.rs)) is the only place chat happens. Both the panel (`send_chat_message`) and the bodies go through `handle_user_message(app, source, text)`. It:
1. takes `chat_lock`, so replies are generated one at a time and history stays in order,
2. saves the user message,
3. builds the prompt with `build_messages`: a system prompt from the persona settings, then the newest history that fits in about 8000 characters / 24 messages,
4. streams the reply with `ollama::chat_stream`,
5. saves the reply and broadcasts it.

Past replies are sent back to the model with their `[emotion: x]` tag so it keeps using the format. `extract_emotion` also accepts a bare `[happy]`, and `visible_stream_text` holds back a possible trailing tag while streaming.

**Storage** ([db.rs](src-tauri/src/db.rs)): SQLite (`rusqlite`, bundled) at `%APPDATA%\com.satyam.diy-waifu\companion.db`. It has `messages(id, ts, role, text, emotion, source)` and `settings(key, value)`. To change the schema, append to `MIGRATIONS` (tracked by `PRAGMA user_version`); never edit an entry that has shipped. [settings.rs](src-tauri/src/settings.rs) is one JSON blob under key `settings`, holding model (default `qwen2.5:3b`), companionName, userName, personality and replyLength. Every field has a serde default, so old saves still load. Logs go to `%LOCALAPPDATA%\com.satyam.diy-waifu\logs` through `tauri-plugin-log`; use `log::info!` and the like, not `println!`.

**Wire protocol** (server → bodies; the broadcast channel goes to **every** client):
- `{"type":"user","text","source"}`: someone said something.
- `{"type":"thinking"}`
- `{"type":"token","text"}`: a streamed piece of the reply.
- `{"type":"reply","emotion","text"}`: the final reply. A message with no `type` also counts as a reply, for older clients.
- `{"type":"state","state"}`: reserved for avatar state.

Emotions are `happy|sad|thinking|excited|neutral`. Bodies must ignore types they don't know. When paused or on an error, a `reply` is still sent so bodies stop waiting.

**Tauri commands** (`invoke`): `send_chat_message`, `get_history`, `clear_history`, `get_settings`, `save_settings` (unloads the old model if it changed), `get_ollama_status`, `list_models`, `pull_model`, `delete_model`, `set_paused`, `get_phone_url`.

**Events** (`listen`): `chat:message` (a saved message), `chat:token`, `chat:busy`, `chat:error`, `settings:changed`, `ollama:status` (`starting|ready|failed`), `model:progress`, `ws:server_status`, `ws:client_connected`, `ws:client_disconnected`, `companion:paused`.

A new command must be added to `generate_handler!` in `lib.rs`. A new plugin permission goes in [src-tauri/capabilities/default.json](src-tauri/capabilities/default.json).

**Frontend:** [src/App.tsx](src/App.tsx) is the shell (header pills and tabs). Each tab is in `src/tabs/` (Chat, Persona, Model, Bodies, System). Shared types live in [src/api.ts](src/api.ts) and mirror the Rust structs. Tabs stay mounted and are only hidden, so a streaming reply or a download carries on when you switch tabs.

The roadmap is in [docs/ROADMAP.md](docs/ROADMAP.md). The milestones are: M1 conversation (done), M2 tray, autostart and desktop pet, M3 screen awareness, M4 long-term memory and speaking first, M5 PC voice, M6 hardening.

## Known gaps

- `monitor.rs` only emits `companion:paused`. It never sets `AppState.paused` or unloads the model (there's a TODO), so auto-pause changes the UI but chat keeps working. `set_paused` is the only thing that actually pauses.
- `setup-ollama.cjs` only handles the Windows zip properly. The macOS/Linux URLs are old single-binary downloads without a runner, and on macOS Tauri puts resources in `Contents/Resources`, not next to the sidecar.

## ESP32 side (`esp32/`)

This is a PlatformIO project (`esp32dev`, Arduino framework) that runs in the Wokwi VS Code extension. The circuit is in `diagram.json`: ILI9341 on CS=15, DC=2, RST=4, with the default VSPI pins.

```sh
cd esp32
~/.platformio/penv/Scripts/pio.exe run     # `pio` isn't on PATH; it comes with the PlatformIO VS Code extension
# then in VS Code: "Wokwi: Start Simulator" (uses esp32/wokwi.toml)
```

- `wokwi.toml` points at `.pio/build/esp32dev/merged-firmware.bin`. This full 4 MB image is produced by `tools/merge_bin.py` after each build. Wokwi needs the merged image because the firmware uses the `huge_app.csv` partition table: the app is about 1.5 MB with GIFs embedded, more than the default 1.2 MB app partition.
- [src/main.cpp](esp32/src/main.cpp) sends `hello` as `esp32`, draws `reply` messages, shows "thinking" on `user`, and ignores `token`. It uses a landscape screen. The GIF plays on the left 200 px, and the emotion and word-wrapped reply go on the right. It connects to `ws://host.wokwi.internal:8080`, the PC as seen from the simulator through Wokwi's built-in gateway. On real hardware, change `WS_HOST` and the Wi-Fi credentials. GIF frames are drawn one per `loop()` so `ws.loop()` keeps running. Don't switch to blocking `playFrame(true, ...)`.
- GIFs are compiled in as headers. To add one, run `python tools/gif2header.py ../gifs/<file>.gif src/gifs/<name>.h gif_<name>`, which scales it to fit 200×240, flattens transparency and writes a `PROGMEM` array. Then point a row of `EMOTION_GIFS` at it. Right now every emotion maps to `gif_wave`. The plan is to move GIF serving to the Tauri app later, and only `gifForEmotion`/`startGif` would need to change.
- `gifs/` at the repo root holds the source animations.

## Phone side (`phone/`)

`phone/` holds plain HTML/CSS/JS with no build step, served by `phone.rs`. It does the same job as the ESP32 firmware: it connects to `ws://<page host>:8080` (override with `?ws=`) and says `hello` as `phone`. It shows the GIF from `EMOTION_GIFS` in `app.js` with the emotion and text, which stream in from `token` messages, and sends `{"text"}` from a text box or hold-to-talk speech. It can also read replies aloud. To add a GIF, add it to `FILES` in `phone.rs` and to `EMOTION_GIFS` in `app.js`. Speech recognition and Wake Lock need a secure context, so they only work over `adb reverse` (localhost) or with Chrome's insecure-origin flag. See `phone/README.md`.
