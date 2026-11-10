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
npm run setup:ollama   # just fetch the pinned, checksummed Ollama sidecar into src-tauri/binaries/
npm run setup:ollama:gpu  # same, but also keep the Vulkan GPU runner (Windows/Linux)
npm run setup:whisper  # just fetch the pinned, SHA-256-checked whisper.cpp server into src-tauri/binaries/whisper/
npm run dev            # frontend only in a browser (Tauri invoke/listen calls will fail)
npm run build          # tsc type-check + vite build (the only frontend "lint")
```

Rust-only checks, run from `src-tauri/`: `cargo check`, `cargo clippy`, `cargo test` (single test: `cargo test parses_emotion_tag_variants`). `cargo test live_chat_streams -- --ignored --nocapture` runs a real chat against a running Ollama. If the dev app is open, the build can't overwrite `target/debug/ollama.exe`; use `CARGO_TARGET_DIR=target/lint` for checks.

The Ollama sidecar lives in `src-tauri/binaries/` (gitignored) and has two parts that must ship together. `ollama-<target-triple>[.exe]` is bundled through `bundle.externalBin`. `lib/ollama/` holds the model runner (`llama-server` plus ggml DLLs) and is bundled through `bundle.resources`. Ollama looks for `lib/ollama` next to its own exe, and Tauri copies resources into `target/debug` in dev. If the runner is missing, Ollama still starts but every `/api/chat` returns 500. `setup-ollama.cjs` pins Ollama `v0.32.4`, checks the archive against the release's `sha256sum.txt`, and copies only the CPU runner, skipping the CUDA, ROCm and MLX folders (and Vulkan unless `--gpu`). It writes `lib/ollama/VERSION`. It handles the Windows zip, the Linux `.tar.zst` and the macOS `.tgz`. On macOS/Linux, bundles keep resources away from the exe, so [tauri.macos.conf.json](src-tauri/tauri.macos.conf.json) / [tauri.linux.conf.json](src-tauri/tauri.linux.conf.json) map the runner to `ollama-runtime/build/lib/ollama/`, and `spawn_ollama` starts the sidecar in `ollama-runtime/` (Ollama also looks in `<cwd>/build/lib/ollama`).

The speech-to-text engine works the same way. `setup-whisper.cjs` downloads whisper.cpp release `b5454` (`whisper-bin-x64.zip`) and checks its hash. It keeps only `whisper-server.exe`, `whisper.dll` and the `ggml*.dll` files in `src-tauri/binaries/whisper/`, which is bundled through `resources` as `whisper/`. It isn't an `externalBin`: `voice.rs` starts it itself with `tokio::process`. Speech models are downloaded at runtime into `%APPDATA%\com.satyam.diy-waifu\whisper\`. Outside Windows there's no download; `voice.rs` uses a `whisper-server` on PATH (e.g. `brew install whisper-cpp`).

Piper (natural voices, [tts.rs](src-tauri/src/tts.rs)) is downloaded at runtime from the Voice tab: the engine (release `2023.11.14-2`, per platform) and voices (`rhasspy/piper-voices` `v1.0.0`), all SHA-256 pinned, into `%APPDATA%\com.satyam.diy-waifu\piper\`.

CI ([.github/workflows/ci.yml](.github/workflows/ci.yml)) runs `npm run build`, `cargo clippy -- -D warnings` and `cargo test` on Windows, Ubuntu and macOS, with empty placeholder sidecar files.

## Architecture

Everything starts in `run()` in [src-tauri/src/lib.rs](src-tauri/src/lib.rs).

Plugins:
- `single-instance` (registered first): a second launch shows the panel instead.
- `global-shortcut`: the push-to-talk key. Its handler calls `voice::on_shortcut` for each press and release.
- `log`
- `shell`
- `autostart`: the entry passes `--minimized`, which skips showing the panel.

The setup hook opens the DB, manages the pairing code and the mDNS advertisement, **then creates the windows** (they're `create: false` in `tauri.conf.json`; created any earlier, their pages call commands before the DB is managed), places and shows the pet, builds the tray, and spawns the long-running async tasks:

1. **WebSocket server** ([websocket.rs](src-tauri/src/websocket.rs)) on `0.0.0.0:8080`. The handshake checks the **pairing code** ([pairing.rs](src-tauri/src/pairing.rs)): clients on this PC (loopback, which includes the pet, `adb reverse` and Wokwi) need nothing, clients on the local network need `?token=<code>`, and public addresses are refused. The code is the `pair_token` setting; making a new one (`new_pairing_code`) disconnects every non-loopback client. Each client gets a `tokio::select!` loop that:
   - forwards every message from the shared `broadcast::Sender<String>` to the client, and
   - parses incoming JSON. `{"type":"hello","body":"phone|esp32|pet"}` names the client, and `{"type":"answer","id","yes"}` answers an "ask me first" question, and `{"text"}` is passed to `companion::handle_user_message` in a spawned task, with the body name as the source.
2. **Ollama watchdog** (`supervise_ollama` in lib.rs):
   - runs `ollama serve` via `tauri-plugin-shell` and waits for `/api/tags`,
   - health-checks it every 15s, and kills and respawns it after two misses in a row,
   - keeps the child in `AppState.ollama_child`, which is killed on `RunEvent::ExitRequested`.
3. **Phone page server** ([phone.rs](src-tauri/src/phone.rs)) on `0.0.0.0:8081`. A minimal HTTP server that sends CORS `*`:
   - the `phone/` pages from its `FILES` table, via `include_bytes!`,
   - the whole repo `gifs/` folder under `/gifs/`, via `include_dir!` (`build.rs` reruns when it changes),
   - for the ESP32 ([esp32.rs](src-tauri/src/esp32.rs)): `/esp32/manifest.json` (the manifest plus `files: {name: {size, id}}`) and `/esp32/gifs/<name>`, each GIF resized to fit 200x240 and flattened onto black with the `image` crate, converted on first request and cached in memory. `esp32::advertise` also publishes `_companion._tcp` / `diy-waifu.local` over mDNS.
4. **Screen awareness** ([activity.rs](src-tauri/src/activity.rs)). Every third tick (~6s), if `settings.autoPause` is on, it calls `pause::set_auto` with a reason while any `settings.heavyApps` process is running (this used to be `monitor.rs`). Every 2s it reads the foreground window and the time since the last input, using the `windows` crate (`cfg(windows)`; stubs elsewhere). It ignores the app's own windows. Then it:
   - **Classifies** the window with `settings.activityRules`. First match wins. A rule's `app` is the exact process name, and `title`/`path` are substrings. Anything matching `privacyRules` becomes `private` with no title.
   - **Tracks presence:** active, idle after 5 min, away after 15 min. Video and gaming never count as away.
   - **Stores** the snapshot in `AppState.activity` and emits `activity:changed`.
   - **Broadcasts** `{"type":"state"}` when the category or away status changes. New WebSocket clients get the current state when they connect.
   - **Records** `activity_sessions` rows: app, category and times, never titles, pruned after 30 days.
   - **Feeds the prompt:** `prompt_context` gives the brain one system-prompt line. Window titles go in only if `settings.shareTitles` is on.
5. **Speaking first** ([proactive.rs](src-tauri/src/proactive.rs)): runs every 20s.
   - **Due reminders** are always delivered, even in quiet hours. While paused they go out as plain text, because the model is unloaded.
   - **With `settings.proactive` on**, it picks one moment at a time: a greeting about 30s after launch, a welcome back after 20+ min away, a break nudge after `breakAfterMinutes` of coding or creative work, or a check-in after 3h of silence.
   - **It stays quiet:**
     - in quiet hours,
     - within `proactiveGapMinutes` of its last line,
     - while you're gaming, watching video, in a private window, idle or away,
     - within 10 min of a chat,
     - while paused.
   - **It speaks** through `companion::speak_first`, which skips if a chat is running (reminders wait instead) and saves with source `proactive`.

6. **Pet click pass-through** (`desktop::pass_through_loop`): every 50ms, compares the cursor with the hit rectangles the pet page reports (`set_pet_hit_rects`: the GIF, controls, bubble and chat box) and toggles `set_ignore_cursor_events`, so clicks on the see-through part of the pet window reach the app behind it. Skipped while click-through mode is on.
7. **Hands-free** ([handsfree.rs](src-tauri/src/handsfree.rs)), plus the whisper idle reaper (see Voice).

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

Shared state is `AppState { broadcast_tx, ollama_child, ollama_status, paused, pause, shutting_down, activity, chat_lock }`, plus a managed `db::Db` and `desktop::PetState`.

**The brain** ([companion.rs](src-tauri/src/companion.rs)) is the only place chat happens. Both the panel (`send_chat_message`) and the bodies go through `handle_user_message(app, source, text)`. It:
1. takes `chat_lock`, so replies are generated one at a time and history stays in order,
2. saves the user message,
3. handles "remember that…" / "forget…" (`memory::parse_command`) and "remind me in 20 min / at 5pm to…" (`reminders::parse_request`) **before** the model, and passes the result in as a note,
4. builds the prompt with `build_messages`:
   - a system prompt from the persona settings,
   - context lines: date, activity, memory recall, the internet line, and the notes,
   - the newest history that fits in about 8000 characters / `HISTORY_MAX_MESSAGES` (24),
5. streams the reply with `ollama::chat_stream`, running tool calls in between,
6. saves the reply, broadcasts it, and calls `memory::after_exchange`.

**Memory** ([memory.rs](src-tauri/src/memory.rs)):
- **Facts:**
  - **Stored** in the `facts` table with origin `auto` or `user`.
  - **In the prompt:** all of them up to 12. Past that, the best matches by meaning, topped up with the newest, 8 in all.
  - **Learned** by `after_exchange` every 4 user messages, in the background with the chat model (`chat_once`, JSON mode). Near-duplicates (cosine ≥ 0.9) are skipped.
- **Summaries:** once 20+ messages have scrolled out of the 24-message window, they're summarized, and the latest two summaries go in the prompt.
- **Recall:** up to 3 older messages that match by meaning.
- **Embeddings:** "by meaning" uses `nomic-embed-text`, which the user downloads from the Memory tab. Without it, everything works except meaning-based matching. Vectors are f32 BLOBs, with `search_document:` / `search_query:` prefixes, and missing ones are backfilled.

**Reminders** ([reminders.rs](src-tauri/src/reminders.rs)) are set by the regex parser or by the model's `set_reminder` tool, which `generate_reply` handles itself.

**Voice** ([voice.rs](src-tauri/src/voice.rs)):
- **Push-to-talk:** hold `settings.pttShortcut` (default Ctrl+Shift+Space) or the pet's 🎤 button (`voice_start` / `voice_stop`).
- **Recording:** cpal records the default microphone on its own thread (cpal streams aren't `Send`), for 60s at most.
- **Speech-to-text:** on release, the audio is downmixed to mono, resampled to 16 kHz and made into a WAV. It goes to `whisper-server` at `127.0.0.1:8178` via `/inference`. The server starts on first use and is stopped after 10 idle minutes.
- **Result:** noise like `[BLANK_AUDIO]` is dropped. The text goes to `handle_user_message` with source `voice`.
- **Status:** `voice:state` reports `idle|listening|transcribing|error`.
- **Speaking replies:** done in the pet window when `settings.speakReplies` is on, through `speakReply` in [src/speech.ts](src/speech.ts). `settings.ttsEngine` picks WebView2's `speechSynthesis` (`windows`, the Windows voices) or `piper`: `piper_speak` runs `piper --model <voice> --output_file` with the text on stdin and returns the WAV, which the pet plays (falling back to a Windows voice on any error). Starting to listen cancels speech.
- **Hands-free** (`settings.handsFree`, off by default): a thread keeps the mic open and runs `handsfree::Vad` (an energy threshold that follows the noise floor; 90ms of speech starts an utterance, 800ms of silence ends it). Each utterance is transcribed, and only text starting with the wake phrase (`settings.wakeWord`, default "hey <companionName>"; the greeting word is optional and the name fuzzy) is sent, with the phrase cut off. Just the phrase waits 8s for the request. Everything else is dropped unlogged. It runs only while voice input is on, a model is downloaded, and the companion isn't paused. `voice:hands_free` reports on/off.

**Internet tools** ([tools/](src-tauri/src/tools/)): with `settings.internetAccess` on, the brain sends Ollama a `tools` list. With `settings.askBeforeLookup` on, each lookup first asks yes/no through [ask.rs](src-tauri/src/ask.rs): `chat:ask` to the panel and `{"type":"ask","id","text"}` to every body, answered by `answer_ask` or a body's `answer` message (first answer wins, 60s timeout means no), then `chat:ask_done` / `ask_done`. A "no" becomes a tool result telling the model not to guess. If the model returns `tool_calls`, the brain runs them, adds `role: "tool"` results, and asks again. After `MAX_TOOL_ROUNDS` (3) it has to answer. Models that report "does not support tools" are remembered and get plain chat.
- `web_search`: the **Brave Search API** when `settings.braveApiKey` is set, otherwise DuckDuckGo's official **Instant Answer API** (quick facts only).
  - **Don't scrape search engines' HTML pages.** DuckDuckGo and Google serve automated clients a bot challenge, and getting around it is off-limits.
  - Google's Custom Search API is closed to new users and shuts down in January 2027.
- `fetch_page`: readable text, capped at 4000 characters. Every hop goes through `guard::resolve_public`, the SSRF guard: it refuses localhost, private, link-local and CGNAT addresses after DNS lookup, and the request is pinned to the address it checked. Redirects are followed by hand.
- `weather`: Open-Meteo.

Tool output from the web is labelled untrusted. `ToolCall::arg` accepts renamed arguments, because small models write `city` for `place`. Replies keep their `sources` (a JSON column in `messages`).

The system prompt also gets the local date and time from chrono, plus an internet on/off line.

Past replies are sent back to the model with their `[emotion: x]` tag so it keeps using the format. `extract_emotion` also accepts a bare `[happy]`, and `visible_stream_text` holds back a possible trailing tag while streaming.

**Storage** ([db.rs](src-tauri/src/db.rs)): SQLite (`rusqlite`, bundled) at `%APPDATA%\com.satyam.diy-waifu\companion.db`. It has:
- `messages(id, ts, role, text, emotion, source, sources)`
- `settings(key, value)`
- `activity_sessions(app, category, start_ts, end_ts)`
- `facts(text, origin, created, last_used, embedding)`
- `summaries(from_id, to_id, text)`
- `message_embeddings(message_id, embedding)`
- `reminders(due_ts, text, done)`

`clear_messages` also clears summaries and message embeddings. To change the schema, append to `MIGRATIONS` (tracked by `PRAGMA user_version`); never edit an entry that has shipped. [settings.rs](src-tauri/src/settings.rs) is one JSON blob under key `settings`, holding model (default `qwen2.5:3b`), companionName, userName, personality, replyLength, autoPause, heavyApps, petVisible, screenAwareness, shareTitles, activityRules, privacyRules, internetAccess, braveApiKey, askBeforeLookup, memoryLearning, proactive, proactiveGapMinutes, breakAfterMinutes, quietStart, quietEnd, voiceInput, whisperModel, pttShortcut, handsFree, wakeWord, speakReplies, ttsVoice, ttsRate, ttsPitch, ttsEngine and piperVoice. The pairing code (`pair_token`) and pet position (`pet_position`) are separate keys. Every field has a serde default, so old saves still load. Logs go to `%LOCALAPPDATA%\com.satyam.diy-waifu\logs` through `tauri-plugin-log`; use `log::info!` and the like, not `println!`.

**Wire protocol** (server → bodies; the broadcast channel goes to **every** client):
- `{"type":"user","text","source"}`: someone said something.
- `{"type":"thinking"}`
- `{"type":"tool","name","label"}`: a lookup is running, e.g. "searching: …". Bodies drop any text streamed before it.
- `{"type":"token","text"}`: a streamed piece of the reply.
- `{"type":"reply","emotion","text","sources"?}`: the final reply. A message with no `type` also counts as a reply, for older clients.
- `{"type":"ask","id","text"}` / `{"type":"ask_done","id"}`: an "ask me first" question and its end. Bodies with buttons send `{"type":"answer","id","yes"}`.
- `{"type":"state","state"}`: avatar state. `away` and `active` set presence. Anything else is the activity category: `coding|video|gaming|music|browsing|chatting|creative|other|private`, or `idle` for none.

Emotions are `happy|sad|thinking|excited|neutral`. Bodies must ignore types they don't know. When paused or on an error, a `reply` is still sent so bodies stop waiting.

**Tauri commands** (`invoke`):
- Chat: `send_chat_message`, `get_history`, `clear_history`.
- Settings: `get_settings`, `save_settings` (unloads the old model if it changed).
- Models: `get_ollama_status`, `list_models`, `pull_model`, `delete_model`.
- Pause: `set_paused` (manual), `get_pause_state`.
- Activity: `get_activity`, `get_activity_totals(since)`.
- Internet: `test_search(braveKey)`, `answer_ask(id, yes)`.
- Voice: `voice_status`, `download_voice_model(id)`, `delete_voice_model(id)`, `voice_start`, `voice_stop`, `piper_status`, `download_piper_engine`, `download_piper_voice(id)`, `delete_piper_voice(id)`, `piper_speak(text, voice?)` (raw WAV bytes).
- Memory: `memory_status`, `list_facts`, `add_fact`, `update_fact`, `delete_fact`, `list_summaries`, `delete_summary`, `list_reminders`, `delete_reminder`, `clear_memory`, `export_memory`, `import_memory(json)`.
- Desktop: `get_desktop_state`, `set_pet_visible`, `set_pet_click_through`, `set_pet_hit_rects(rects)`, `set_autostart`, `open_panel`, `get_gif_manifest`.
- Bodies: `get_pairing` (code, LAN IP, phone URL with `?token=`, QR SVG), `new_pairing_code`.

**Events** (`listen`): `chat:message` (a saved message), `chat:token`, `chat:tool` (lookup label), `chat:busy`, `chat:error`, `settings:changed`, `ollama:status` (`starting|ready|failed`), `model:progress`, `ws:server_status`, `ws:client_connected`, `ws:client_disconnected`, `companion:paused` (`PauseState`), `pet:click_through`, `activity:changed` (`Activity`), `memory:changed` (refresh the Memory tab), `voice:state`, `voice:progress`, `voice:transcript`, `voice:hands_free`, `chat:ask` (`{id, text}`), `chat:ask_done` (id).

A new command must be added to `generate_handler!` in `lib.rs`. A new plugin permission goes in [src-tauri/capabilities/default.json](src-tauri/capabilities/default.json).

**Frontend:** Vite builds two pages, `index.html` (the panel) and `pet.html`.
- **Panel:** [src/App.tsx](src/App.tsx) is the shell (header pills and tabs). Each tab is in `src/tabs/` (Chat, Persona, Memory, Voice, Model, Activity, Bodies, System). The Persona tab also holds the speaking-first settings. Shared types live in [src/api.ts](src/api.ts) and mirror the Rust structs. Tabs stay mounted and are only hidden, so a streaming reply or a download carries on when you switch tabs.
- **Pet:** [src/pet/Pet.tsx](src/pet/Pet.tsx) is a body like the phone. It connects to `ws://127.0.0.1:8080` as `pet`. You drag it by the GIF (`data-tauri-drag-region`), and on hover it shows a chat button and a panel button. Replies appear in a speech bubble that hides after a while.

**Avatar state machine** ([phone/avatar.js](phone/avatar.js), typed by `avatar.d.ts`) is shared by the pet and the phone page. Priority, from highest:
1. reply emotion (for 8s)
2. talking
3. thinking
4. sleeping (away)
5. activity
6. idle

`gifFor` maps a state to a file through [gifs/manifest.json](gifs/manifest.json), with fallbacks such as excited → happy → idle → default. **To add art**, drop the GIF in `gifs/` and point a state at it in the manifest. Nothing else changes, and a test checks that every file the manifest names exists. The ESP32 picks the new art up too, the next time it connects.

The roadmap is in [docs/ROADMAP.md](docs/ROADMAP.md). Every milestone (M1 to M7) is built; only the avatar art (4.6) is left, and that's the user's.

## Known gaps

- The macOS/Linux paths (Ollama runner location, whisper from PATH, Piper builds) are written but only checked by CI compiling them, not run. Screen awareness is Windows-only (stubs elsewhere). On Linux the global hotkey needs X11.
- Hands-free uses a simple energy detector, so a loud room makes it transcribe more (more CPU), though only wake-phrase speech is used.

## ESP32 side (`esp32/`)

This is a PlatformIO project (`esp32dev` board, Arduino core 3.x) with two envs:
- `esp32dev`: real hardware. On first boot, WiFiManager opens a hotspot `Companion-Setup` asking for Wi-Fi, the pairing code and an optional PC address (saved in NVS via `Preferences`). With no address it finds the PC with mDNS (`_companion._tcp`, then `diy-waifu.local`). Holding BOOT for 3s forgets everything.
- `wokwi` (`-DWOKWI`): the Wokwi simulator, with fixed `Wokwi-GUEST` Wi-Fi and `ws://host.wokwi.internal:8080`, which arrives as loopback so no code is needed. `wokwi.toml` points at this env's build.

The circuit is in `diagram.json`: ILI9341 on CS=15, DC=2, RST=4, with the default VSPI pins.

```sh
cd esp32
~/.platformio/penv/Scripts/pio.exe run                         # builds both envs; `pio` isn't on PATH, it comes with the PlatformIO VS Code extension
~/.platformio/penv/Scripts/pio.exe run -e esp32dev -t upload   # flash a real board
# Wokwi: "Wokwi: Start Simulator" (uses esp32/wokwi.toml)
```

- `tools/merge_bin.py` writes the full 4 MB `merged-firmware.bin` after each build. Wokwi needs it because the firmware uses the `huge_app.csv` partition table (3 MB app, ~900 KB LittleFS).
- [src/main.cpp](esp32/src/main.cpp) sends `hello` as `esp32` and follows the same avatar priority as `avatar.js` (reply emotion for 8s, talking, searching, thinking, sleeping when away, activity, idle, with the same fallbacks). It shows replies, lookups and "ask" questions as text. Landscape screen: GIF on the left 200 px, emotion and word-wrapped text on the right.
- **GIFs:** on each WebSocket connect, `syncGifs` fetches `http://<pc>:8081/esp32/manifest.json`, downloads missing files into LittleFS as `/g/<id>.gif`, deletes ones no longer named, then plays from flash through AnimatedGIF's file callbacks. The compiled-in `gif_wave` (made with `tools/gif2header.py`) is the fallback when a file is missing or there's no room. GIF frames are drawn one per `loop()` so `ws.loop()` keeps running. Don't switch to blocking `playFrame(true, ...)`.
- `gifs/` at the repo root holds the source animations.

## Phone side (`phone/`)

`phone/` holds plain HTML/CSS/JS with no build step, served by `phone.rs`. It does the same job as the ESP32 firmware: it connects to `ws://<page host>:8080` (override with `?ws=`) and says `hello` as `phone`. It shows the GIF chosen by the shared `avatar.js` state machine (from `gifs/manifest.json`), with the emotion and the text, which streams in from `token` messages. It sends `{"text"}` from a text box or hold-to-talk speech, can read replies aloud, and shows yes/no buttons for "ask me first" questions. It reads the pairing code from `?token=` (the Bodies tab QR code), keeps it in localStorage, removes it from the address bar, and adds it to the WebSocket URL. `app.js` is an ES module (`<script type="module">`). New page files must be added to `FILES` in `phone.rs`. Speech recognition and Wake Lock need a secure context, so they only work over `adb reverse` (localhost) or with Chrome's insecure-origin flag. See `phone/README.md`.
