# diy-waifu: full companion roadmap

## Context

Today the app can carry a message to the model and back. The local LLM (Ollama sidecar) answers single messages with an emotion tag, and the reply goes out over WebSocket to a phone page and an ESP32 screen. The goal is an **always-on desktop companion** with:
- real conversation that keeps history,
- memory that lasts across sessions,
- a GIF avatar that floats on the desktop and reacts to the app you're using,
- later, voice on the PC.

Decisions already made:
- **Model:** you can pick it; the default is `qwen2.5:3b`.
- **Avatar:** a floating desktop pet, with the phone and ESP32 bodies kept.
- **Screen awareness:** the app in front plus its window title, with no screenshots.
- **PC voice:** a later phase.

Sizes: **S** is under half a day, **M** is 1–2 days, **L** is 3+ days.

---

## Phase 0: Groundwork (do first, small)

0.1 **S** Commit the current uncommitted work (`phone/`, the PlatformIO ESP32 project, `phone.rs`, `CLAUDE.md`) as a baseline.

0.2 **M** Create one "brain" entry point. The Ollama call, the reply broadcast and the emotion parsing are currently duplicated in `send_chat_message` ([lib.rs](src-tauri/src/lib.rs)) and the WebSocket handler ([websocket.rs](src-tauri/src/websocket.rs)).
- New module `src-tauri/src/companion.rs` with `handle_user_message(app, source, text)`. Both callers use it.
- A `tokio::Mutex` makes chats run one at a time, so history stays in order.
- Keep `extract_emotion` and its test in `ollama.rs`.

0.3 **S** Define wire protocol v2 now, so later phases don't keep changing the bodies.
- Server → body:
  - `{"type":"reply","emotion","text"}`. A message with no `type` is still a reply, so old clients keep working.
  - `{"type":"thinking"}`
  - `{"type":"token","text"}` (streaming; bodies may ignore it)
  - `{"type":"state","state"}` (avatar state)
  - `{"type":"user","text","source"}` (echoes what was said)
- Body → server:
  - `{"text"}`
  - `{"type":"hello","body":"phone|esp32|pet"}`
- Update [phone/app.js](phone/app.js) `ws.onmessage` and the ESP32 [main.cpp](esp32/src/main.cpp) to ignore types they don't know (right now the phone treats every message as a reply).

0.4 **S** File logging with `tauri-plugin-log`, so problems can be debugged once the app is always on.

## Phase 1: Real conversation (biggest gain in how it feels)

1.1 **M** **Storage.** Add `rusqlite` (`bundled` feature) and create `src-tauri/src/db.rs`. The database lives at `app_data_dir()/companion.db`.
- Tables:
  - `messages(id, ts, role, text, emotion, source)`
  - `settings(key, value)`
- Then later phases add:
  - `facts`
  - `summaries`
  - `activity_sessions`
- Simple migrations, keyed by a `user_version` number.

1.2 **M** **Conversation history.**
- `ollama::chat` takes a full `Vec<ChatMessage>` instead of one user message.
- `companion.rs` builds the prompt from:
  - system prompt (persona),
  - [memory block: added in Phase 5],
  - [activity context: added in Phase 3],
  - the last ~12 turns, cut to fit the token budget,
  - the new message.
- Set `options.num_ctx` to 4096. Save both the user's message and the reply.

1.3 **M** **Streaming replies.**
- Use `stream: true` against `/api/chat`, reusing the line-by-line stream parsing in `pull_model`.
- Emit `chat:token` to the UI and the pet. Broadcast `thinking` first and `reply` at the end. The ESP32 only needs the final reply.
- Strip the emotion tag from the final text only.

1.4 **S** **Persona settings.**
- Replace the hardcoded `SYSTEM_PROMPT` with a template filled from settings:
  - companion name,
  - personality text,
  - the user's name,
  - reply length,
  - emotion-tag instructions (always added).
- Keep "2–4 sentences" as the default length.

1.5 **M** **Model picker.**
- `MODEL_NAME` becomes a setting, default `qwen2.5:3b`.
- New commands:
  - `list_models` (the existing `/api/tags`, reusing `TagsResponse`),
  - `set_model`,
  - pull with progress: emit `model:progress {completed,total}` from the existing `PullProgress` struct, with those fields added.
- Install, uninstall and status take a model name.

1.6 **S** **Optional GPU runner.** Add a flag to `setup-ollama.cjs` that also copies the Vulkan runner, so 3B+ models run faster on any GPU.

1.7 **M** **Control panel rework** ([src/App.tsx](src/App.tsx)): split into tabs.
- **Chat:** history loaded from the DB, streaming text.
- **Persona**
- **Model**
- **Bodies:** the phone URL and connected clients.
- **System**

Add new commands for these: `get_history`, `clear_history`, `get_settings`, `set_setting`.

## Phase 2: Always-on behaviour

2.1 **M** **Tray icon.** Use the `tray-icon` feature that's already enabled. Menu items:
- Open panel
- Show/hide pet
- Pause/resume
- Click-through pet
- Quit

2.2 **S** **Hide instead of quit.** Closing the main window hides it (`CloseRequested` → `prevent_close` + `hide`). Only Quit in the tray exits, which still kills Ollama through the existing `ExitRequested` handler.

2.3 **S** **Start with Windows.**
- Add the `autostart:default` permission to [capabilities/default.json](src-tauri/capabilities/default.json) and the `@tauri-apps/plugin-autostart` JS package, plus a toggle in System.
- Launch with `--minimized` so it starts in the tray with only the pet showing.

2.4 **S** **Single instance** with `tauri-plugin-single-instance`, so a second launch focuses the panel instead of starting a second app.

2.5 **M** **Ollama watchdog.**
- Health-check `/api/tags` every 30s. If it's down, respawn using `spawn_ollama` and `wait_for_ollama_ready`.
- Emit `ollama:status` and show it in the UI.

2.6 **S** **Make auto-pause real.**
- [monitor.rs](src-tauri/src/monitor.rs) sets `AppState.paused` and calls `ollama::unload_model`.
- Keep manual pause and auto pause separate, so a manual pause always wins.
- The heavy-app list becomes a setting.
- This loop gets merged into the activity loop in 3.1.

## Phase 3: Screen awareness

3.1 **M** **Foreground app tracker.**
- New `src-tauri/src/activity.rs` (replaces `monitor.rs`). Uses the `windows` crate: `GetForegroundWindow`, `GetWindowTextW`, `GetWindowThreadProcessId`, with sysinfo for the process name.
- Poll every 2s and emit `activity:changed {app, title, category}` when it changes.
- Heavy-app pause logic moves here.

3.2 **S** **Idle / away detection** with `GetLastInputInfo`, giving three states: active, idle (5 min), away (15 min).

3.3 **M** **App categories.**
- Rules table: a process name or title pattern maps to a category (coding, gaming, video, music, browsing, chatting, creative, other).
- Ship good defaults (VS Code, Chrome + "YouTube" → video, Steam games, Blender, and so on), stored in settings and editable in the Activity tab.

3.4 **S** **Privacy rules.**
- A global "screen awareness" toggle.
- A blocklist (password managers, private/incognito windows, user-added apps). Blocked titles are never stored or sent to the model.

3.5 **S** **Activity sessions.** Save `activity_sessions(app, category, start, end)` without titles, so it can say things like "you've been coding for 2h".

3.6 **S** **Activity context in prompts.** One line in the system prompt, e.g. "The user is currently in VS Code (coding), for 40 minutes."

3.7 **S** **App → avatar reaction.**
- When the category changes, broadcast `{"type":"state","state":"<category>"}`. This needs no LLM call.
- Bodies map the state to a GIF.

## Phase 4: Desktop pet + avatar art

4.1 **M** **Pet window.**
- A second window `pet` in [tauri.conf.json](src-tauri/tauri.conf.json): `transparent`, `decorations:false`, `alwaysOnTop`, `skipTaskbar`, `shadow:false`, small size.
- Vite multi-page setup: `pet.html` + `src/pet/main.tsx`. Give the pet window its own capability entry.

4.2 **S** **Drag and remember position.** Drag with `startDragging()`, save the position in settings, and restore it on launch. Click-through comes from the tray toggle (`setIgnoreCursorEvents`).

4.3 **M** **Speech bubble and quick chat.**
- The bubble shows streaming tokens and fades after a while.
- Clicking the pet opens a small text box that calls the brain (source `pet`).

4.4 **M** **Avatar state machine**, shared by the pet and the phone. Priority from highest:
1. reply emotion (for ~8s)
2. talking
3. thinking
4. user away → sleeping
5. activity category
6. idle

4.5 **S** **One GIF manifest.**
- `gifs/manifest.json` maps each state or emotion to a file. [phone.rs](src-tauri/src/phone.rs) serves the whole `gifs/` folder (switch to the `include_dir` crate instead of listing files by hand), plus the manifest.
- The pet and the phone both load `http://localhost:8081/gifs/…`, which removes the duplicated `EMOTION_GIFS` tables.

4.6 **L (art, your task)** **Make the GIFs.**
- One each for: idle, talking, thinking, happy, sad, excited, sleeping, greet/wave.
- Optional ones for activities: coding, gaming, watching.
- Until then, the code falls back to the existing `mai-san-wave.gif`.

4.7 **M** **ESP32 GIFs.**
- Convert the main 5–6 with `tools/gif2header.py`, keeping within the `huge_app` partition size, and handle `state` messages in `main.cpp`.
- Streaming GIFs from :8081 is deferred to Phase 8.

## Phase 5: Long-term memory

5.1 **M** **Rolling summaries.** When stored history goes past the prompt window, a background task summarises the oldest unsummarised turns into `summaries`. The latest summary goes into the prompt.

5.2 **M** **Fact extraction.**
- After each exchange (background, rate-limited), ask the model for durable facts about the user as JSON, e.g. "name is Satyam", "likes Blender", "exam on Friday".
- Skip duplicates and store them in `facts(id, text, created, last_used, embedding)`.

5.3 **M** **Embedding search.**
- Pull `nomic-embed-text` (about 270 MB) and call Ollama `/api/embed`. Store the vectors as BLOBs in SQLite and compare them with cosine similarity in Rust, which is fine up to tens of thousands of rows.
- For each message, put the top-k relevant facts and old messages into the memory block.

5.4 **S** **Explicit memory commands.** "remember that …" and "forget …" are detected before the LLM call.

5.5 **M** **Memory tab.**
- List, edit and delete facts.
- View summaries.
- Clear all memory.
- Export/import a JSON file.

## Phase 6: Speaking first (proactive)

6.1 **M** **Trigger scheduler** (`proactive.rs`). Triggers:
- greeting on startup or when you come back from away,
- a long session in one category (a break reminder after 2h of coding),
- switching to a game or video,
- time of day,
- a long silence.

6.2 **S** **Limits.**
- At most one line every N minutes.
- Quiet hours.
- Do-not-disturb while a heavy app or a fullscreen app is running.
- An on/off toggle and a frequency slider.

6.3 **S** **Generating the line.** The LLM gets the trigger and the context, the line is saved with source `companion`, and it is broadcast like any reply.

6.4 **M** **Reminders.** "remind me in 20 min to …". The model pulls out `{when, what}` as JSON, it's stored in a `reminders` table, and the scheduler fires it.

## Phase 7: Voice on the PC

7.1 **L** **Speech-to-text.**
- Capture the mic with `cpal`, and transcribe with whisper.cpp (`whisper-rs`, base/small model downloaded on first use).
- A push-to-talk global hotkey (`tauri-plugin-global-shortcut`) and a mic button on the pet.

7.2 **L** **Text-to-speech.**
- A Piper sidecar with a downloadable voice, played with `rodio`. The pet shows "talking" while audio plays.
- A setting chooses where audio plays: PC, phone browser, or both.

7.3 **M** **Hands-free (optional).** Voice activity detection plus a wake word (openWakeWord) for talking without a hotkey.

## Phase 8: Bodies, safety, quality

8.1 **M** **ESP32 on real hardware.**
- WiFiManager captive portal for Wi-Fi credentials.
- The PC advertises `diy-waifu.local` over mDNS (`mdns-sd` crate), so `WS_HOST` isn't hardcoded.
- Reconnect logic.

8.2 **S** **Pairing token.**
- The WebSocket and phone servers need `?token=`, shown in the Bodies tab, ideally as a QR code.
- Optionally limit them to the LAN or localhost.

8.3 **S** **Per-body info.** The `hello` message registers each body's type, which the UI shows. The pet ignores replies that came from itself, to avoid duplicates.

8.4 **M** **Streaming GIFs to the ESP32** from :8081 into PSRAM/SPIFFS, so new art doesn't need a reflash.

8.5 **M** **macOS/Linux.** Fix `setup-ollama.cjs` and the resource path, and make the activity tracker a stub on non-Windows.

8.6 **M** **Tests and CI.**
- Rust unit tests for:
  - prompt building and the token budget,
  - activity rule matching,
  - memory search ranking,
  - protocol parsing.
- A GitHub Actions job running `cargo test` + `npm run build`.

---

## Phase 9: Internet access

The model decides when to look something up, using Ollama tool calling. `qwen2.5` supports it. 3B and up are reliable; 1.5B calls tools but often renames their arguments. All tools are read-only.

> **Changed while building:** DuckDuckGo's HTML results page answers automated clients with a bot challenge, and Google's Custom Search API is closed to new users (it shuts down in January 2027). Search now uses official APIs: the **Brave Search API** with a key from the user (free tier), or keyless **DuckDuckGo Instant Answers** (quick facts only).

9.1 **M** **Tool-calling loop in the brain.**
- `companion.rs` sends a `tools` list with `/api/chat`.
- When the model returns `tool_calls`, it runs them, adds `role: "tool"` results, and asks again, up to 3 rounds. The final round streams as today.
- New `src-tauri/src/tools/` module with one file per tool, each with a schema, a function to run it, and a short status label.

9.2 **M** **`web_search(query)`.**
- The Brave Search API (`X-Subscription-Token`) gives the top 5 results when a key is set. Otherwise it uses `api.duckduckgo.com` Instant Answers (abstract, answer, definition, related topics).
- Results are cached for 10 min per query, and requests are spaced at least 1.1s apart (Brave's free tier allows 1 request per second).
- The System tab has a key field with a Test button (`test_search`).

9.3 **M** **`fetch_page(url)`.**
- Fetches a page and pulls out the readable text (`article`/`main`/`p` blocks, without scripts and navigation), capped at about 4000 characters.
- Limits: http(s) only, 10s timeout, 2 MB max, text/HTML only.
- **SSRF guard:** refuse localhost, private and link-local IPs after DNS resolution. Otherwise the model could reach Ollama on :11434 or the app's own ports.

9.4 **S** **No-network context.**
- Current date, time and weekday go in the system prompt, so the model doesn't search for "what day is it".
- Optional `weather(place)` tool using Open-Meteo, which needs no key.

9.5 **S** **Safety and settings.**
- An "Internet access" toggle, on by default once this ships, and an optional "ask me first" mode. In that mode the panel and pet show "Search the web for X?" with yes/no.
- Page text is untrusted. It's wrapped as tool output with an instruction not to follow commands found in it. The tools can only read, so the worst case is a wrong answer.
- Every lookup is logged.

9.6 **S** **Showing it.**
- Bodies get `{"type":"tool","name","label"}`, e.g. "searching: weather in Delhi". The avatar state is `searching`, which falls back to thinking.
- Replies keep their source links: a new `sources` column in `messages`, added as a migration, shown under the bubble in the panel.

9.7 **S** **Fallbacks.**
- If the model rejects `tools` ("does not support tools"), chat continues without tools and the Model tab shows a note.
- If DDG fails or blocks, the tool returns a clear error, and the model says it couldn't look it up.

9.8 **S** **Tests.**
- The search parser against the fixture.
- The SSRF guard (localhost, 10.x, 192.168.x, IPv6 loopback, and a DNS name resolving to a private IP).
- The tool loop with a fake Ollama response.
- Readable-text extraction.

## Order and milestones

| Milestone | Phases | You get |
|---|---|---|
| M1 "remembers the chat" | 0, 1 | Multi-turn chat, saved history, streaming, persona, model picker |
| M2 "always there" | 2, 4.1–4.5 | Tray, autostart, floating pet with a speech bubble |
| M3 "sees what I do" | 3, 4.6–4.7 | Reacts to apps, idle and sleep states, activity-aware replies |
| M4 "looks things up" | 9 | Web search and page reading through tool calls, shown sources, SSRF-safe |
| M5 "knows me" | 5, 6 | Long-term facts, summaries, proactive lines (can use the web from M4), reminders |
| M6 "talks" | 7 | PC voice in and out |
| M7 "polished" | 8 | Real ESP32, security, cross-platform, CI |

**Status:** everything is built except the avatar art (4.6), which is yours. Things that need real hardware or other OSes are written but untested there (noted below).
- **M7** is done (8.1–8.6):
  - **8.1 ESP32 on real hardware:**
    - A WiFiManager setup hotspot `Companion-Setup` asks for Wi-Fi, the pairing code and an optional PC address.
    - The PC advertises `_companion._tcp` / `diy-waifu.local` over mDNS.
    - Hold BOOT 3s to reset. The `wokwi` env keeps the simulator setup.
    - Builds; not run on a board yet.
  - **8.2 Pairing code:** for every WebSocket client that isn't on this PC, shown in the Bodies tab with a QR code. A new code kicks the others; public addresses are refused.
  - **8.3 Per-body info:** was already done through `hello`.
  - **8.4 GIFs from the PC:**
    - The ESP32 downloads `/esp32/gifs/*` (resized by the app) into LittleFS and follows the same avatar states as the pet.
    - This also covers 4.7: new art reaches the ESP32 without a reflash.
  - **8.5 macOS/Linux:**
    - `setup-ollama.cjs` handles each OS's archive (pinned, checksummed).
    - The runner path goes through `tauri.<os>.conf.json` plus the sidecar's working directory.
    - whisper-server is taken from PATH, and there's a macOS mic permission.
    - Compiled by CI only.
  - **8.6 CI:** GitHub Actions builds the frontend and runs clippy and tests on Windows, Linux and macOS.
- **Leftovers, now done:**
  - 1.6 `npm run setup:ollama:gpu` (Vulkan).
  - 9.5 "ask me first" yes/no in the panel, pet and phone.
  - 7.2 Piper voices, downloaded from the Voice tab.
  - 7.3 Hands-free wake phrase (energy-based voice detection plus whisper, instead of openWakeWord).
  - Pet clicks pass through its see-through area.
  - `monitor.rs` folded into `activity.rs`.
- **M1** is done (0.1–0.4, 1.1–1.7).
- **M2** is done (2.1–2.6, 4.1–4.5). The pet is a WebSocket body. The state machine is shared through `phone/avatar.js`.
- **M6** is done for 7.1, plus 7.2 using Windows voices:
  - **Speech-to-text:** a pinned whisper.cpp server (`b5454`) running as a side process, started when first used. Models are downloaded from the Voice tab.
  - **Push-to-talk:** a global hotkey, or the pet's mic button.
  - **Speaking:** WebView2 `speechSynthesis` in the pet.
  - **Not done yet:** Piper neural voices (the rest of 7.2) and the hands-free wake word (7.3).
- **M5** is done (5.1–5.5, 6.1–6.4):
  - Reminders are parsed from the message first (regex), with a `set_reminder` tool as backup.
  - Fact learning runs every 4 user messages.
  - Proactive lines skip gaming, video, private windows, idle, away, and the 10 min after a chat.
- **M4** is done (9.1–9.4, 9.6–9.8). The 9.5 "ask me first" mode isn't built yet; there's only the on/off switch. The search source changed (see Phase 9).
- **M3** is done for 3.1–3.7. `monitor.rs` stays as its own 5s heavy-app loop rather than being merged into `activity.rs`. 4.6 (art) is yours; 4.7 (ESP32 GIFs) waits for that art.

## Critical files

- **Modified:**
  - [src-tauri/src/lib.rs](src-tauri/src/lib.rs)
  - [ollama.rs](src-tauri/src/ollama.rs)
  - [websocket.rs](src-tauri/src/websocket.rs)
  - [monitor.rs](src-tauri/src/monitor.rs) (becomes `activity.rs`)
  - [phone.rs](src-tauri/src/phone.rs)
  - [Cargo.toml](src-tauri/Cargo.toml)
  - [tauri.conf.json](src-tauri/tauri.conf.json)
  - [capabilities/default.json](src-tauri/capabilities/default.json)
  - [src/App.tsx](src/App.tsx)
  - [phone/app.js](phone/app.js)
  - [esp32/src/main.cpp](esp32/src/main.cpp)
  - `setup-ollama.cjs`
- **New:** `companion.rs`, `db.rs`, `activity.rs`, `memory.rs`, `proactive.rs`, `pet.html` + `src/pet/`, `gifs/manifest.json`
- **Reused:**
  - `extract_emotion`
  - `pull_model` stream parsing
  - `unload_model`
  - `wait_for_ollama_ready` / `spawn_ollama`
  - `TagsResponse`
  - the broadcast channel in `AppState`
  - `lan_ip()`

## Verification (per milestone)

- **Always:**
  - `cargo test` and `cargo clippy` in `src-tauri/`,
  - `npm run build`,
  - `npm run tauri:dev`, with the phone page open at the URL from the Bodies tab.
- **M1:**
  - Say "my name is X", then ask "what's my name?". It answers correctly, and still does after an app restart because history comes from SQLite.
  - Tokens stream in the panel, and the phone and ESP32 still get the final reply and emotion.
  - Switching the model to 1.5B and back works.
- **M2:**
  - Closing the window leaves the tray icon and the pet.
  - After a reboot with autostart on, it comes up minimised.
  - Killing `ollama.exe` in Task Manager → the watchdog restarts it within about 30s.
  - Opening Blender actually blocks chat.
- **M3:**
  - Focusing VS Code, then YouTube, then the desktop changes the pet's GIF within about 2s.
  - Being idle for 5+ minutes makes it sleep.
  - A blocklisted app never shows up in logs or the DB.
- **M4 (internet):**
  - "What's the weather in Delhi today?" and "who won yesterday's match?" trigger a search, the pet shows "searching", and the reply lists its sources.
  - Asking it to fetch `http://localhost:11434` or `http://192.168.1.1` is refused.
  - With the internet toggle off, it says it can't look things up.
- **M5 (memory):**
  - A fact from days ago is recalled.
  - The Memory tab edit and delete work.
  - "remind me in 1 min" fires.
  - Proactive lines respect the frequency limit and quiet hours.
- **M6 (voice):**
  - Hold the hotkey → speech is transcribed → the spoken reply plays, and the pet shows "talking".
