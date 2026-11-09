# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A desktop "AI companion brain" built with Tauri 2 (Rust backend) + React 19 / Vite / TypeScript frontend. It runs a local LLM through a bundled Ollama sidecar and serves replies over a WebSocket to "bodies" that show text and an emotion. There are three bodies, and all can be connected at once:
- a floating desktop pet window (`src/pet/`),
- an ESP32 with an ILI9341 display (`esp32/`),
- an Android phone running a web page (`phone/`).

It lives in the tray. Closing a window only hides it, and Quit in the tray menu exits.

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

Everything starts in `run()` in [src-tauri/src/lib.rs](src-tauri/src/lib.rs).

Plugins:
- `single-instance` (registered first): a second launch shows the panel instead.
- `log`
- `shell`
- `autostart`: the entry passes `--minimized`, which skips showing the panel.

The setup hook opens the DB, places and shows the pet, builds the tray, and spawns four long-running async tasks:

1. **WebSocket server** ([websocket.rs](src-tauri/src/websocket.rs)) on `0.0.0.0:8080`. Each client gets a `tokio::select!` loop that:
   - forwards every message from the shared `broadcast::Sender<String>` to the client, and
   - parses incoming JSON. `{"type":"hello","body":"phone|esp32|pet"}` names the client, and `{"text"}` is passed to `companion::handle_user_message` in a spawned task, with the body name as the source.
2. **Ollama watchdog** (`supervise_ollama` in lib.rs):
   - runs `ollama serve` via `tauri-plugin-shell` and waits for `/api/tags`,
   - health-checks it every 15s, and kills and respawns it after two misses in a row,
   - keeps the child in `AppState.ollama_child`, which is killed on `RunEvent::ExitRequested`.
3. **Phone page server** ([phone.rs](src-tauri/src/phone.rs)) on `0.0.0.0:8081`. A minimal HTTP server that sends CORS `*`:
   - the `phone/` pages from its `FILES` table, via `include_bytes!`,
   - the whole repo `gifs/` folder under `/gifs/`, via `include_dir!` (`build.rs` reruns when it changes).
4. **Heavy-app monitor** ([monitor.rs](src-tauri/src/monitor.rs)): every 5s, if `settings.autoPause` is on, calls `pause::set_auto` with a reason while any `settings.heavyApps` process is running.

**Pausing** ([pause.rs](src-tauri/src/pause.rs)): `PauseState { paused, manual, autoReason }`. Manual and auto pause are tracked separately, so auto-resume never undoes a manual pause. Only `pause::set_manual` and `pause::set_auto` change it. Each change:
- updates `AppState.paused` (which the brain checks),
- emits `companion:paused` with the full state,
- syncs the tray,
- unloads the model when it goes from running to paused.

**Desktop** ([desktop.rs](src-tauri/src/desktop.rs)):
- **Tray:** Open panel, Show pet, Click-through pet, Pause, Quit. Left-click opens the panel. Check marks are re-synced from the real state with `sync_tray`.
- **Windows:** `main` (the panel) and `pet`, both `visible: false` in `tauri.conf.json` and shown from setup.
- **Pet position:** saved under setting key `pet_position`, debounced from `WindowEvent::Moved`. On launch it's restored if still on a monitor, otherwise put bottom-right of the work area.
- **Click-through** (`set_ignore_cursor_events`) is deliberately not saved.

Shared state is `AppState { broadcast_tx, ollama_child, ollama_status, paused, pause, shutting_down, chat_lock }`, plus a managed `db::Db` and `desktop::PetState`.

**The brain** ([companion.rs](src-tauri/src/companion.rs)) is the only place chat happens. Both the panel (`send_chat_message`) and the bodies go through `handle_user_message(app, source, text)`. It:
1. takes `chat_lock`, so replies are generated one at a time and history stays in order,
2. saves the user message,
3. builds the prompt with `build_messages`: a system prompt from the persona settings, then the newest history that fits in about 8000 characters / 24 messages,
4. streams the reply with `ollama::chat_stream`,
5. saves the reply and broadcasts it.

Past replies are sent back to the model with their `[emotion: x]` tag so it keeps using the format. `extract_emotion` also accepts a bare `[happy]`, and `visible_stream_text` holds back a possible trailing tag while streaming.

**Storage** ([db.rs](src-tauri/src/db.rs)): SQLite (`rusqlite`, bundled) at `%APPDATA%\com.satyam.diy-waifu\companion.db`. It has `messages(id, ts, role, text, emotion, source)` and `settings(key, value)`. To change the schema, append to `MIGRATIONS` (tracked by `PRAGMA user_version`); never edit an entry that has shipped. [settings.rs](src-tauri/src/settings.rs) is one JSON blob under key `settings`, holding model (default `qwen2.5:3b`), companionName, userName, personality, replyLength, autoPause, heavyApps and petVisible. Every field has a serde default, so old saves still load. Logs go to `%LOCALAPPDATA%\com.satyam.diy-waifu\logs` through `tauri-plugin-log`; use `log::info!` and the like, not `println!`.

**Wire protocol** (server → bodies; the broadcast channel goes to **every** client):
- `{"type":"user","text","source"}`: someone said something.
- `{"type":"thinking"}`
- `{"type":"token","text"}`: a streamed piece of the reply.
- `{"type":"reply","emotion","text"}`: the final reply. A message with no `type` also counts as a reply, for older clients.
- `{"type":"state","state"}`: avatar state. `away` and `active` set presence; anything else is an activity (Phase 3).

Emotions are `happy|sad|thinking|excited|neutral`. Bodies must ignore types they don't know. When paused or on an error, a `reply` is still sent so bodies stop waiting.

**Tauri commands** (`invoke`):
- Chat: `send_chat_message`, `get_history`, `clear_history`.
- Settings: `get_settings`, `save_settings` (unloads the old model if it changed).
- Models: `get_ollama_status`, `list_models`, `pull_model`, `delete_model`.
- Pause: `set_paused` (manual), `get_pause_state`.
- Desktop: `get_desktop_state`, `set_pet_visible`, `set_pet_click_through`, `set_autostart`, `open_panel`, `get_gif_manifest`.
- Bodies: `get_phone_url`.

**Events** (`listen`): `chat:message` (a saved message), `chat:token`, `chat:busy`, `chat:error`, `settings:changed`, `ollama:status` (`starting|ready|failed`), `model:progress`, `ws:server_status`, `ws:client_connected`, `ws:client_disconnected`, `companion:paused` (`PauseState`), `pet:click_through`.

A new command must be added to `generate_handler!` in `lib.rs`. A new plugin permission goes in [src-tauri/capabilities/default.json](src-tauri/capabilities/default.json).

**Frontend:** Vite builds two pages, `index.html` (the panel) and `pet.html`.
- **Panel:** [src/App.tsx](src/App.tsx) is the shell (header pills and tabs). Each tab is in `src/tabs/` (Chat, Persona, Model, Bodies, System). Shared types live in [src/api.ts](src/api.ts) and mirror the Rust structs. Tabs stay mounted and are only hidden, so a streaming reply or a download carries on when you switch tabs.
- **Pet:** [src/pet/Pet.tsx](src/pet/Pet.tsx) is a body like the phone. It connects to `ws://127.0.0.1:8080` as `pet`. You drag it by the GIF (`data-tauri-drag-region`), and on hover it shows a chat button and a panel button. Replies appear in a speech bubble that hides after a while.

**Avatar state machine** ([phone/avatar.js](phone/avatar.js), typed by `avatar.d.ts`) is shared by the pet and the phone page. Priority, from highest:
1. reply emotion (for 8s)
2. talking
3. thinking
4. sleeping (away)
5. activity
6. idle

`gifFor` maps a state to a file through [gifs/manifest.json](gifs/manifest.json), with fallbacks such as excited → happy → idle → default. **To add art**, drop the GIF in `gifs/` and point a state at it in the manifest. Nothing else changes, and a test checks that every file the manifest names exists. The ESP32 still uses its own compiled-in table.

The roadmap is in [docs/ROADMAP.md](docs/ROADMAP.md). The milestones are: M1 conversation (done), M2 tray, autostart and desktop pet (done), M3 screen awareness, M4 long-term memory and speaking first, M5 PC voice, M6 hardening.

## Known gaps

- Clicks on the pet window's transparent area are caught by the window, not passed to what's underneath (only click-through mode passes them).
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

`phone/` holds plain HTML/CSS/JS with no build step, served by `phone.rs`. It does the same job as the ESP32 firmware: it connects to `ws://<page host>:8080` (override with `?ws=`) and says `hello` as `phone`. It shows the GIF chosen by the shared `avatar.js` state machine (from `gifs/manifest.json`), with the emotion and the text, which streams in from `token` messages. It sends `{"text"}` from a text box or hold-to-talk speech, and can read replies aloud. `app.js` is an ES module (`<script type="module">`). New page files must be added to `FILES` in `phone.rs`. Speech recognition and Wake Lock need a secure context, so they only work over `adb reverse` (localhost) or with Chrome's insecure-origin flag. See `phone/README.md`.
