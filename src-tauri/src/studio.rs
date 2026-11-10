//! Studio voices: the heavy, most lifelike option. Qwen3-TTS 1.7B CustomVoice
//! (Apache-2.0; English, Japanese, Chinese, Korean and 6 more languages, with a
//! speaking-style instruction), run by the audio.cpp server
//! (https://github.com/0xShug0/audio.cpp, Apache-2.0).
//!
//! The server build (~32 MB for CPU, ~84 MB with GPU support through Vulkan)
//! and the model (~2.8 GB) are downloaded from the Voice tab into
//! `<app data>/studio/`, pinned and SHA-256 checked. The app writes the
//! server's config, starts it on first use at 127.0.0.1:8179, and stops it
//! after 10 idle minutes, like the whisper server. The model stays loaded
//! between sentences, so only the very first one waits for it.

use crate::db::Db;
use crate::settings::Settings;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const PORT: u16 = 8179;
const RELEASE: &str = "v0.9.4";
const IDLE_SHUTDOWN: Duration = Duration::from_secs(10 * 60);
/// audio-cpp/audio.cpp-gguf at a fixed commit
const MODEL_REV: &str = "73ce4162f6cce370dd533c6782b772135c1b2a8e";
const MODEL_PATH: &str = "Qwen3-TTS-12Hz-1.7B-CustomVoice-GGUF/qwen3-tts-12hz-1.7b-customvoice-q8_0.gguf";
const MODEL_FILE: &str = "qwen3-tts-12hz-1.7b-customvoice-q8_0.gguf";
const MODEL_SHA256: &str = "c8ac3f94b6e73f137c0e7ecc9270bc8faec7a476ff799b87e48844d1d0987ac3";
const MODEL_MB: u32 = 2817;
/// The id the model has in the server config
const MODEL_ID: &str = "qwen3";

struct Build {
    asset: &'static str,
    sha256: &'static str,
    size_mb: u32,
    /// audio.cpp backend name
    backend: &'static str,
}

/// This platform's CPU build and GPU build (macOS: one Metal build for both)
fn builds() -> Option<(Build, Build)> {
    let b = |asset, sha256, size_mb, backend| Build { asset, sha256, size_mb, backend };
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some((
            b("audio-v0.9.4-bin-windows-x64-cpu.zip", "596ef1d6ed5113f3a4f5194c8c00e02be5748d7dfb09e7f270168e6ec5c8e0cf", 32, "cpu"),
            b("audio-v0.9.4-bin-windows-x64-vulkan.zip", "804f70cf47c7b8450ef37e76fc04ecee71728345061f6e749a412932dd7db7a7", 84, "vulkan"),
        )),
        ("linux", "x86_64") => Some((
            b("audio-v0.9.4-bin-ubuntu-x64-cpu.tar.gz", "13529ee5366daeb13546190ba25e8118f46bece9c8811e7eb113c41bd121a41a", 64, "cpu"),
            b("audio-v0.9.4-bin-ubuntu-x64-vulkan.tar.gz", "e5d8af8eb6500d87eb2ef94c1bc52be7dfc63b26012ff9abf0adce6f1f960208", 117, "vulkan"),
        )),
        ("macos", "aarch64") => {
            let metal = || b("audio-v0.9.4-bin-macos-arm64-metal.tar.gz", "5722c4f33e56114345965c0dee472a9239218f7499ace6393bba6a951a41b86a", 37, "metal");
            Some((metal(), metal()))
        }
        ("macos", "x86_64") => {
            let metal = || b("audio-v0.9.4-bin-macos-x64-metal.tar.gz", "be2a35aa004cc3d63978c77723ee3f2805129eedf726172c45145ef114664451", 39, "metal");
            Some((metal(), metal()))
        }
        _ => None,
    }
}

fn build_for(gpu: bool) -> Option<Build> {
    builds().map(|(cpu, gpu_build)| if gpu { gpu_build } else { cpu })
}

pub struct Voice {
    pub id: &'static str,
    pub note: &'static str,
}

/// Qwen3-TTS CustomVoice's built-in speakers. Every voice can speak every language.
pub const VOICES: &[Voice] = &[
    Voice { id: "ono_anna", note: "Japanese female, playful and bright (recommended)" },
    Voice { id: "vivian", note: "Female, bright and young (Chinese native)" },
    Voice { id: "serena", note: "Female, warm and gentle (Chinese native)" },
    Voice { id: "sohee", note: "Female, warm (Korean native)" },
    Voice { id: "aiden", note: "Male, sunny American English" },
    Voice { id: "ryan", note: "Male, lively English" },
    Voice { id: "dylan", note: "Male, young (Beijing Mandarin native)" },
    Voice { id: "eric", note: "Male, bright (Sichuan native)" },
    Voice { id: "uncle_fu", note: "Male, low and mellow (Chinese native)" },
];

pub fn is_voice(id: &str) -> bool {
    VOICES.iter().any(|v| v.id == id)
}

pub const DEFAULT_STYLE: &str = "Speak naturally and expressively, like a cheerful close friend chatting casually.";

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("studio");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn engine_dir(app: &AppHandle, gpu: bool) -> Option<PathBuf> {
    Some(root(app).ok()?.join(if gpu { "engine-gpu" } else { "engine-cpu" }))
}

/// Finds the server program wherever the archive put it
fn find_exe(dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "audiocpp_server.exe" } else { "audiocpp_server" };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == name) {
                return Some(p);
            }
        }
    }
    None
}

fn engine_exe(app: &AppHandle, gpu: bool) -> Option<PathBuf> {
    find_exe(&engine_dir(app, gpu)?)
}

fn model_file(app: &AppHandle) -> Option<PathBuf> {
    let p = root(app).ok()?.join("models").join(MODEL_FILE);
    p.is_file().then_some(p)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceInfo {
    id: &'static str,
    note: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    supported: bool,
    /// The build for the current CPU/GPU setting is downloaded
    engine_ready: bool,
    model_ready: bool,
    engine_mb: u32,
    model_mb: u32,
    running: bool,
    voices: Vec<VoiceInfo>,
}

pub fn status(app: &AppHandle) -> Status {
    let s = Settings::load(&app.state::<Db>());
    let running = app.state::<StudioState>().server.try_lock().map(|g| g.is_some()).unwrap_or(true);
    Status {
        supported: builds().is_some(),
        engine_ready: engine_exe(app, s.studio_gpu).is_some(),
        model_ready: model_file(app).is_some(),
        engine_mb: build_for(s.studio_gpu).map_or(0, |b| b.size_mb),
        model_mb: MODEL_MB,
        running,
        voices: VOICES.iter().map(|v| VoiceInfo { id: v.id, note: v.note }).collect(),
    }
}

/// Downloads whatever is missing for the current setting: the server build, then the model.
pub async fn download(app: &AppHandle) -> Result<(), String> {
    let gpu = Settings::load(&app.state::<Db>()).studio_gpu;
    let build = build_for(gpu).ok_or("Studio voices have no build for this computer")?;
    let root = root(app)?;

    if engine_exe(app, gpu).is_none() {
        let archive = root.join(build.asset);
        let url = format!("https://github.com/0xShug0/audio.cpp/releases/download/{RELEASE}/{}", build.asset);
        crate::tts::download(app, &url, &archive, Some(build.sha256), "studio").await?;
        let dir = engine_dir(app, gpu).ok_or("No app data folder")?;
        let unpacked = crate::tts::unpack(&archive, &dir).await;
        tokio::fs::remove_file(&archive).await.ok();
        if unpacked.is_err() || engine_exe(app, gpu).is_none() {
            return Err("Couldn't unpack the studio voice engine".into());
        }
        log::info!("audio.cpp {RELEASE} ({}) installed", build.backend);
    }

    if model_file(app).is_none() {
        let dir = root.join("models");
        tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
        let url = format!("https://huggingface.co/audio-cpp/audio.cpp-gguf/resolve/{MODEL_REV}/{MODEL_PATH}");
        crate::tts::download(app, &url, &dir.join(MODEL_FILE), Some(MODEL_SHA256), "studio").await?;
        log::info!("Studio voice model installed");
    }
    Ok(())
}

pub async fn delete(app: &AppHandle) -> Result<(), String> {
    stop_server(app).await;
    // Give Windows a moment to release the files of the stopped server
    tokio::time::sleep(Duration::from_millis(500)).await;
    tokio::fs::remove_dir_all(root(app)?).await.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

struct Server {
    child: tokio::process::Child,
    /// What it was started with; a change restarts it
    key: String,
}

pub struct StudioState {
    server: tokio::sync::Mutex<Option<Server>>,
    last_used: std::sync::Mutex<Instant>,
}

impl Default for StudioState {
    fn default() -> Self {
        Self { server: tokio::sync::Mutex::new(None), last_used: std::sync::Mutex::new(Instant::now()) }
    }
}

/// Paths in the config use forward slashes, which audio.cpp asks for on Windows too
fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn server_config(model: &Path, backend: &str, voice: &str) -> serde_json::Value {
    let threads = std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 8));
    serde_json::json!({
        "host": "127.0.0.1",
        "port": PORT,
        "backend": backend,
        "device": 0,
        "threads": threads,
        "lazy_load": true,
        "max_loaded_models": 1,
        "ui": false,
        "ui_management": false,
        "models": [{
            "id": MODEL_ID,
            "family": "qwen3_tts",
            "path": slashes(model),
            "task": "tts",
            "mode": "offline",
            "default_voice_preset": { "voice_id": voice }
        }]
    })
}

async fn server_answers() -> bool {
    reqwest::Client::new()
        .get(format!("http://127.0.0.1:{PORT}/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

async fn ensure_server(app: &AppHandle, s: &Settings) -> Result<(), String> {
    let model = model_file(app).ok_or("The studio voice isn't downloaded yet (Voice tab)")?;
    let exe = engine_exe(app, s.studio_gpu).ok_or("The studio voice engine isn't downloaded yet (Voice tab)")?;
    let backend = build_for(s.studio_gpu).map_or("cpu", |b| b.backend);
    let key = format!("{}|{backend}|{}", exe.display(), s.studio_voice);

    let state = app.state::<StudioState>();
    let mut slot = state.server.lock().await;
    *state.last_used.lock().unwrap() = Instant::now();
    if let Some(srv) = slot.as_mut() {
        if matches!(srv.child.try_wait(), Ok(None)) && srv.key == key {
            return Ok(());
        }
        srv.child.start_kill().ok();
        *slot = None;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let config = root(app)?.join("server.json");
    let json = serde_json::to_vec_pretty(&server_config(&model, backend, &s.studio_voice)).map_err(|e| e.to_string())?;
    tokio::fs::write(&config, json).await.map_err(|e| e.to_string())?;
    let log_file = root(app)?.join("server.log");

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("--config")
        .arg(&config)
        .arg("--log")
        .arg("--log-file")
        .arg(&log_file)
        .current_dir(exe.parent().unwrap_or(&exe))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let child = cmd.spawn().map_err(|e| format!("Couldn't start the studio voice: {e}"))?;
    log::info!("Started audio.cpp server ({backend}); its log is {}", log_file.display());
    *slot = Some(Server { child, key });

    for _ in 0..240 {
        if server_answers().await {
            return Ok(());
        }
        if let Some(srv) = slot.as_mut() {
            if let Ok(Some(status)) = srv.child.try_wait() {
                *slot = None;
                let hint = if backend == "vulkan" { " If your GPU drivers are old, try the CPU build." } else { "" };
                return Err(format!("The studio voice stopped while starting ({status}).{hint}"));
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("The studio voice didn't start in time".into())
}

async fn stop_server(app: &AppHandle) {
    if let Some(mut s) = app.state::<StudioState>().server.lock().await.take() {
        s.child.start_kill().ok();
        log::info!("Stopped audio.cpp server");
    }
}

/// For app exit: kill without waiting
pub fn kill_server_now(app: &AppHandle) {
    if let Ok(mut slot) = app.state::<StudioState>().server.try_lock() {
        if let Some(s) = slot.as_mut() {
            s.child.start_kill().ok();
        }
    }
}

/// Stops the server after a while unused, to give its (few GB of) RAM back
pub async fn idle_reaper(app: AppHandle) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let idle = app.state::<StudioState>().last_used.lock().unwrap().elapsed();
        if idle >= IDLE_SHUTDOWN {
            stop_server(&app).await;
        }
    }
}

/// The language code to ask for: the reply language if one is set, else let the model tell
fn language(s: &Settings) -> &str {
    match s.language.as_str() {
        "en" | "ja" => &s.language,
        _ => "auto",
    }
}

fn request(text: &str, voice: &str, lang: &str, style: Option<&str>) -> serde_json::Value {
    let mut options = serde_json::json!({ "voice_id": voice, "language": lang });
    if let Some(style) = style {
        options["instruct"] = style.into();
    }
    serde_json::json!({ "model": MODEL_ID, "input": text, "response_format": "wav", "options": options })
}

async fn post(body: &serde_json::Value) -> Result<Vec<u8>, String> {
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{PORT}/v1/audio/speech"))
        .json(body)
        // The first request loads the 2.8 GB model
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| format!("The studio voice failed: {e}"))?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes[..bytes.len().min(400)]).to_string();
        log::warn!("audio.cpp {status}: {detail}");
        return Err(format!("The studio voice failed ({status})"));
    }
    if !bytes.starts_with(b"RIFF") {
        return Err("The studio voice didn't return audio".into());
    }
    Ok(bytes.to_vec())
}

/// Speaks `text` with `voice_id` (or the saved voice) and returns a WAV file.
pub async fn synthesize(app: &AppHandle, text: &str, voice_id: Option<&str>) -> Result<Vec<u8>, String> {
    let s = Settings::load(&app.state::<Db>());
    let text = crate::tts::speakable(text);
    if !text.chars().any(char::is_alphanumeric) {
        return Err("Nothing to say".into());
    }
    let voice = voice_id.filter(|v| is_voice(v)).unwrap_or(&s.studio_voice);
    ensure_server(app, &s).await?;
    let style = s.studio_style.trim();
    let result = if style.is_empty() {
        post(&request(&text, voice, language(&s), None)).await
    } else {
        match post(&request(&text, voice, language(&s), Some(style))).await {
            Ok(wav) => Ok(wav),
            Err(e) => {
                // Don't let a style the model rejects silence it
                log::warn!("Studio voice failed with the style instruction, retrying without: {e}");
                post(&request(&text, voice, language(&s), None)).await
            }
        }
    };
    *app.state::<StudioState>().last_used.lock().unwrap() = Instant::now();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_matches_the_audio_cpp_example() {
        let c = server_config(Path::new(r"C:\Users\me\studio\models\q.gguf"), "vulkan", "ono_anna");
        assert_eq!(c["port"], PORT);
        assert_eq!(c["backend"], "vulkan");
        let m = &c["models"][0];
        assert_eq!(m["family"], "qwen3_tts");
        assert_eq!(m["path"], "C:/Users/me/studio/models/q.gguf");
        assert_eq!(m["default_voice_preset"]["voice_id"], "ono_anna");
    }

    #[test]
    fn requests_carry_voice_language_and_style() {
        let r = request("こんにちは", "ono_anna", "ja", Some("Cheerful."));
        assert_eq!(r["model"], MODEL_ID);
        assert_eq!(r["options"]["voice_id"], "ono_anna");
        assert_eq!(r["options"]["language"], "ja");
        assert_eq!(r["options"]["instruct"], "Cheerful.");
        assert!(request("hi", "ryan", "auto", None)["options"].get("instruct").is_none());
    }

    #[test]
    fn every_platform_build_is_pinned() {
        if let Some((cpu, gpu)) = builds() {
            for b in [cpu, gpu] {
                assert_eq!(b.sha256.len(), 64);
                assert!(b.asset.starts_with("audio-v0.9.4-"));
            }
        }
        assert!(VOICES.iter().all(|v| v.id.chars().all(|c| c.is_ascii_lowercase() || c == '_')));
    }
}
