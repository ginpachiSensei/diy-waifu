//! Talking to the companion on the PC: push-to-talk speech-to-text.
//!
//! Hold the hotkey (settings.ptt_shortcut) or the pet's mic button: `start`
//! records from the default microphone (cpal, on its own thread). On release,
//! `stop_and_send` turns the audio into 16 kHz mono WAV, sends it to a local
//! whisper.cpp server, and passes the text to the brain like any other message.
//!
//! The whisper server (bundled in `<resources>/whisper/`, see setup-whisper.cjs)
//! starts on first use and stops after 10 idle minutes to free its RAM. Speech
//! models are downloaded from the Voice tab into `<app data>/whisper/`.
//! Speaking replies out loud happens in the pet window (Windows voices).

use crate::settings::Settings;
use crate::db::Db;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::StreamExt;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const WHISPER_PORT: u16 = 8178;
const TARGET_RATE: u32 = 16_000;
const MIN_SECONDS: f32 = 0.4;
const MAX_SECONDS: u64 = 60;
const IDLE_SHUTDOWN: Duration = Duration::from_secs(10 * 60);

/// (id, file, size in MB, note)
pub const MODELS: &[(&str, &str, u32, &str)] = &[
    ("tiny.en", "ggml-tiny.en.bin", 75, "Fastest, English only, makes more mistakes"),
    ("base.en", "ggml-base.en.bin", 142, "Recommended: quick and accurate for English"),
    ("small.en", "ggml-small.en.bin", 466, "More accurate English, slower on CPU"),
    ("base", "ggml-base.bin", 142, "Many languages (Hindi too), less accurate"),
];

fn model_file(id: &str) -> Option<&'static str> {
    MODELS.iter().find(|m| m.0 == id).map(|m| m.1)
}

/// English-only models are told the language; multilingual ones detect it
fn language_for(id: &str) -> &'static str {
    if id.ends_with(".en") {
        "en"
    } else {
        "auto"
    }
}

pub(crate) struct Captured {
    pub(crate) samples: Vec<f32>,
    pub(crate) rate: u32,
    pub(crate) channels: u16,
}

/// A running microphone stream, its sample buffer, sample rate and channels
pub(crate) type OpenStream = (cpal::Stream, Arc<Mutex<Vec<f32>>>, u32, u16);

struct Recorder {
    stop: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<Result<Captured, String>>,
}

struct Server {
    child: tokio::process::Child,
    model: PathBuf,
}

pub struct VoiceState {
    recorder: Mutex<Option<Recorder>>,
    server: tokio::sync::Mutex<Option<Server>>,
    last_used: Mutex<Instant>,
}

impl Default for VoiceState {
    fn default() -> Self {
        Self {
            recorder: Mutex::new(None),
            server: tokio::sync::Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
        }
    }
}

#[derive(Serialize, Clone)]
struct StateEvent<'a> {
    /// "idle" | "listening" | "transcribing" | "error"
    state: &'a str,
    error: Option<String>,
}

pub(crate) fn set_state(app: &AppHandle, state: &str, error: Option<String>) {
    if let Some(e) = &error {
        log::warn!("Voice: {e}");
    }
    app.emit("voice:state", StateEvent { state, error }).ok();
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

fn models_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("whisper");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub fn model_path(app: &AppHandle, id: &str) -> Option<PathBuf> {
    let p = models_dir(app).ok()?.join(model_file(id)?);
    p.exists().then_some(p)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    id: &'static str,
    size_mb: u32,
    note: &'static str,
    downloaded: bool,
}

pub fn list_models(app: &AppHandle) -> Vec<ModelInfo> {
    MODELS
        .iter()
        .map(|(id, _, size_mb, note)| ModelInfo {
            id,
            size_mb: *size_mb,
            note,
            downloaded: model_path(app, id).is_some(),
        })
        .collect()
}

/// Downloads a whisper model from Hugging Face, emitting `voice:progress`.
pub async fn download_model(app: &AppHandle, id: &str) -> Result<(), String> {
    let file = model_file(id).ok_or("Unknown speech model")?;
    let dir = models_dir(app)?;
    let part = dir.join(format!("{file}.part"));
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}");

    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Download failed: {}", resp.status()));
    }
    let total = resp.content_length();
    let mut out = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let (mut done, mut last_emit) = (0u64, Instant::now());
    let mut head: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download failed: {e}"))?;
        if head.len() < 4 {
            head.extend(chunk.iter().take(4 - head.len()));
        }
        tokio::io::AsyncWriteExt::write_all(&mut out, &chunk)
            .await
            .map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        if last_emit.elapsed() > Duration::from_millis(250) {
            last_emit = Instant::now();
            app.emit("voice:progress", serde_json::json!({ "model": id, "completed": done, "total": total }))
                .ok();
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut out).await.map_err(|e| e.to_string())?;
    drop(out);

    // whisper.cpp model files start with the "ggml" magic number (stored little-endian)
    if head != b"lmgg" && head != b"ggml" {
        tokio::fs::remove_file(&part).await.ok();
        return Err("The download wasn't a whisper model file".into());
    }
    tokio::fs::rename(&part, dir.join(file)).await.map_err(|e| e.to_string())?;
    app.emit("voice:progress", serde_json::json!({ "model": id, "completed": done, "total": total, "done": true }))
        .ok();
    log::info!("Downloaded speech model {id}");
    Ok(())
}

pub async fn delete_model(app: &AppHandle, id: &str) -> Result<(), String> {
    if let Some(p) = model_path(app, id) {
        // The server may have it open
        stop_server(app).await;
        tokio::fs::remove_file(p).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

pub fn is_recording(app: &AppHandle) -> bool {
    app.state::<VoiceState>().recorder.lock().unwrap().is_some()
}

pub fn input_device_name() -> Option<String> {
    cpal::default_host().default_input_device()?.name().ok()
}

/// Starts recording. Does nothing if already recording (key repeat).
pub fn start(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<VoiceState>();
    let mut slot = state.recorder.lock().unwrap();
    if slot.is_some() {
        return Ok(());
    }
    let settings = Settings::load(&app.state::<Db>());
    if !settings.voice_input {
        return Err("Voice input is off (Voice tab)".into());
    }
    if model_path(app, &settings.whisper_model).is_none() {
        return Err("Download a speech model in the Voice tab first".into());
    }

    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    // cpal streams can't move between threads, so the stream lives on this one
    let thread = std::thread::spawn(move || -> Result<Captured, String> {
        let (stream, buf, rate, channels) = match open_mic() {
            Ok(v) => {
                ready_tx.send(Ok(())).ok();
                v
            }
            Err(e) => {
                ready_tx.send(Err(e.clone())).ok();
                return Err(e);
            }
        };
        // Until released, or the length cap
        stop_rx.recv_timeout(Duration::from_secs(MAX_SECONDS)).ok();
        drop(stream);
        let samples = std::mem::take(&mut *buf.lock().unwrap());
        Ok(Captured {
            samples,
            rate,
            channels,
        })
    });

    match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("The microphone didn't start".into()),
    }
    *slot = Some(Recorder { stop: stop_tx, thread });
    drop(slot);
    set_state(app, "listening", None);
    Ok(())
}

/// Opens the default microphone. Samples collect in the returned buffer. The
/// stream must stay on the thread that opened it (cpal streams aren't Send).
pub(crate) fn open_mic() -> Result<OpenStream, String> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or("No microphone found")?;
    let config = device.default_input_config().map_err(|e| e.to_string())?;
    let rate = config.sample_rate().0;
    let channels = config.channels();
    let buf = Arc::new(Mutex::new(Vec::<f32>::new()));
    let err_fn = |e| log::warn!("Microphone error: {e}");
    let b = buf.clone();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &_| b.lock().unwrap().extend_from_slice(data),
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _: &_| {
                b.lock().unwrap().extend(data.iter().map(|s| *s as f32 / i16::MAX as f32))
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _: &_| {
                b.lock().unwrap().extend(data.iter().map(|s| (*s as f32 - 32768.0) / 32768.0))
            },
            err_fn,
            None,
        ),
        other => return Err(format!("Unsupported microphone format {other:?}")),
    }
    .map_err(|e| format!("Couldn't open the microphone: {e}"))?;
    stream.play().map_err(|e| format!("Couldn't start the microphone: {e}"))?;
    Ok((stream, buf, rate, channels))
}

/// Mono, then linear resampling to 16 kHz (fine for speech)
pub(crate) fn to_16k_mono(c: &Captured) -> Vec<f32> {
    let ch = c.channels.max(1) as usize;
    let mono: Vec<f32> = c
        .samples
        .chunks(ch)
        .map(|f| f.iter().sum::<f32>() / f.len() as f32)
        .collect();
    if c.rate == TARGET_RATE || mono.is_empty() {
        return mono;
    }
    let ratio = c.rate as f64 / TARGET_RATE as f64;
    let out_len = (mono.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos as usize;
            let frac = (pos - j as f64) as f32;
            let a = mono[j];
            let b = *mono.get(j + 1).unwrap_or(&a);
            a + (b - a) * frac
        })
        .collect()
}

/// 16-bit PCM WAV
fn wav_bytes(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    w
}

/// Whisper's markers for "nothing said", e.g. "[BLANK_AUDIO]", "(music)"
pub fn is_noise(text: &str) -> bool {
    let t = text.trim();
    if !t.chars().any(char::is_alphanumeric) {
        return true;
    }
    // One bracketed tag and nothing else
    let wrapped = (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')'));
    wrapped && !t[1..t.len() - 1].contains(['[', '(', ']', ')'])
}

/// Stops recording, transcribes, and sends the text to the brain.
pub async fn stop_and_send(app: &AppHandle) {
    let Some(rec) = app.state::<VoiceState>().recorder.lock().unwrap().take() else {
        return;
    };
    rec.stop.send(()).ok();
    let captured = match tauri::async_runtime::spawn_blocking(move || rec.thread.join()).await {
        Ok(Ok(Ok(c))) => c,
        Ok(Ok(Err(e))) => return set_state(app, "error", Some(e)),
        _ => return set_state(app, "error", Some("Recording failed".into())),
    };
    let audio = to_16k_mono(&captured);
    if (audio.len() as f32 / TARGET_RATE as f32) < MIN_SECONDS {
        return set_state(app, "idle", None);
    }
    set_state(app, "transcribing", None);
    match transcribe(app, &audio).await {
        Ok(text) if !is_noise(&text) => {
            set_state(app, "idle", None);
            log::info!("Heard: {text}");
            app.emit("voice:transcript", &text).ok();
            // Errors are already shown in chat and on the bodies
            let _ = crate::companion::handle_user_message(app, "voice", &text).await;
        }
        Ok(_) => set_state(app, "idle", None),
        Err(e) => set_state(app, "error", Some(e)),
    }
}

// ---------------------------------------------------------------------------
// Whisper server
// ---------------------------------------------------------------------------

fn server_exe(app: &AppHandle) -> Result<PathBuf, String> {
    let p = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("whisper")
        .join(if cfg!(windows) { "whisper-server.exe" } else { "whisper-server" });
    if p.exists() {
        return Ok(p);
    }
    // macOS/Linux: no prebuilt download, so use one from the system
    // (`brew install whisper-cpp`, or a distro package) if it's on PATH
    if !cfg!(windows) {
        if let Some(found) = std::env::var_os("PATH")
            .iter()
            .flat_map(std::env::split_paths)
            .map(|dir| dir.join("whisper-server"))
            .find(|c| c.is_file())
        {
            return Ok(found);
        }
        return Err("Speech-to-text needs whisper-server: install whisper.cpp (e.g. brew install whisper-cpp)".into());
    }
    Err("The speech-to-text engine isn't installed (run npm run setup:whisper)".into())
}

async fn server_answers() -> bool {
    reqwest::Client::new()
        .get(format!("http://127.0.0.1:{WHISPER_PORT}/"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

/// Starts the whisper server with `model` if it isn't already running with it.
async fn ensure_server(app: &AppHandle, model: PathBuf) -> Result<(), String> {
    let state = app.state::<VoiceState>();
    let mut slot = state.server.lock().await;
    *state.last_used.lock().unwrap() = Instant::now();
    if let Some(s) = slot.as_mut() {
        let alive = matches!(s.child.try_wait(), Ok(None));
        if alive && s.model == model {
            return Ok(());
        }
        s.child.start_kill().ok();
        *slot = None;
    }

    let exe = server_exe(app)?;
    let threads = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(2, 8))
        .unwrap_or(4);
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("-m")
        .arg(&model)
        .args(["--host", "127.0.0.1", "--port", &WHISPER_PORT.to_string()])
        .args(["-t", &threads.to_string(), "-nt"])
        .current_dir(exe.parent().unwrap_or(&exe))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // No console window
        cmd.creation_flags(0x0800_0000);
    }
    let child = cmd.spawn().map_err(|e| format!("Couldn't start speech-to-text: {e}"))?;
    log::info!("Started whisper server with {}", model.display());
    *slot = Some(Server { child, model });

    // Loading the model takes a moment
    for _ in 0..120 {
        if server_answers().await {
            return Ok(());
        }
        if let Some(s) = slot.as_mut() {
            if let Ok(Some(status)) = s.child.try_wait() {
                *slot = None;
                return Err(format!("Speech-to-text stopped while starting ({status})"));
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("Speech-to-text didn't start in time".into())
}

async fn stop_server(app: &AppHandle) {
    if let Some(mut s) = app.state::<VoiceState>().server.lock().await.take() {
        s.child.start_kill().ok();
        log::info!("Stopped whisper server");
    }
}

/// For app exit: kill without waiting
pub fn kill_server_now(app: &AppHandle) {
    if let Ok(mut slot) = app.state::<VoiceState>().server.try_lock() {
        if let Some(s) = slot.as_mut() {
            s.child.start_kill().ok();
        }
    }
}

/// Stops the server after a while unused, to give its RAM back
pub async fn idle_reaper(app: AppHandle) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let idle = app.state::<VoiceState>().last_used.lock().unwrap().elapsed();
        if idle >= IDLE_SHUTDOWN && !is_recording(&app) {
            stop_server(&app).await;
        }
    }
}

pub(crate) async fn transcribe(app: &AppHandle, audio: &[f32]) -> Result<String, String> {
    let settings = Settings::load(&app.state::<Db>());
    let model = model_path(app, &settings.whisper_model).ok_or("The speech model is missing")?;
    ensure_server(app, model).await?;

    let part = reqwest::multipart::Part::bytes(wav_bytes(audio, TARGET_RATE))
        .file_name("speech.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json")
        .text("temperature", "0.0")
        .text("language", language_for(&settings.whisper_model));
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{WHISPER_PORT}/inference"))
        .multipart(form)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| format!("Speech-to-text failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Speech-to-text failed: {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    *app.state::<VoiceState>().last_used.lock().unwrap() = Instant::now();
    Ok(v["text"].as_str().unwrap_or_default().trim().to_string())
}

// ---------------------------------------------------------------------------
// Push-to-talk hotkey
// ---------------------------------------------------------------------------

/// (Re-)registers the push-to-talk hotkey from settings. Returns a problem, if any.
pub fn register_shortcut(app: &AppHandle) -> Option<String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let gs = app.global_shortcut();
    gs.unregister_all().ok();
    let settings = Settings::load(&app.state::<Db>());
    let key = settings.ptt_shortcut.trim();
    if !settings.voice_input || key.is_empty() {
        return None;
    }
    match gs.register(key) {
        Ok(()) => {
            log::info!("Push-to-talk on {key}");
            None
        }
        Err(e) => {
            let msg = format!("Couldn't use {key} as the talk key ({e}). Pick another in the Voice tab.");
            set_state(app, "error", Some(msg.clone()));
            Some(msg)
        }
    }
}

/// Called by the global-shortcut plugin for every press and release
pub fn on_shortcut(app: &AppHandle, pressed: bool) {
    if pressed {
        if let Err(e) = start(app) {
            set_state(app, "error", Some(e));
        }
    } else {
        let app = app.clone();
        tauri::async_runtime::spawn(async move { stop_and_send(&app).await });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmixes_and_resamples() {
        // 0.1 s of stereo at 48 kHz -> 0.1 s mono at 16 kHz
        let c = Captured {
            samples: vec![0.5; 4800 * 2],
            rate: 48_000,
            channels: 2,
        };
        let out = to_16k_mono(&c);
        assert_eq!(out.len(), 1600);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn writes_a_valid_wav_header() {
        let w = wav_bytes(&[0.0, 1.0, -1.0], 16_000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(i16::from_le_bytes([w[46], w[47]]), i16::MAX);
    }

    #[test]
    fn recognizes_whisper_noise_markers() {
        assert!(is_noise("[BLANK_AUDIO]"));
        assert!(is_noise(" (music) "));
        assert!(is_noise("..."));
        assert!(is_noise(""));
        assert!(!is_noise("What's the weather like?"));
        assert!(!is_noise("(laughs) that was funny [really]"));
    }

    #[test]
    fn languages_follow_the_model() {
        assert_eq!(language_for("base.en"), "en");
        assert_eq!(language_for("base"), "auto");
    }
}
