//! Kokoro voices: Kokoro-82M (https://huggingface.co/hexgrad/Kokoro-82M,
//! Apache-2.0), run by sherpa-onnx (https://github.com/k2-fsa/sherpa-onnx,
//! Apache-2.0). Much livelier than Piper, still offline and on the CPU.
//!
//! The engine (~27 MB) and the int8 model with all its voices (~132 MB) are
//! downloaded from the Voice tab into `<app data>/kokoro/`, pinned and SHA-256
//! checked. `synthesize` runs `sherpa-onnx-offline-tts` once per sentence and
//! returns the WAV; the pet plays it (settings.tts_engine == "kokoro").
//!
//! sherpa-onnx 2.x reads pronunciations from lexicons, and this model ships
//! only US/UK English (and Chinese) ones, so these are English voices. Piper
//! stays for Hindi.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager};

const ENGINE_RELEASE: &str = "v2.0.1";
const MODEL: &str = "kokoro-int8-multi-lang-v1_0";
const MODEL_SHA256: &str = "72c347230107ef8a31098ec4d468a605244beef5f1ed00db97efa13518800653";
const MODEL_FILES: &[&str] = &["model.int8.onnx", "voices.bin", "tokens.txt", "lexicon-us-en.txt", "lexicon-gb-en.txt"];

/// (release asset without .tar.bz2, sha256) for this platform
fn engine_asset() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        // MT = the C++ runtime is built in, so no Visual C++ redistributable is needed
        ("windows", "x86_64") => Some(("sherpa-onnx-v2.0.1-win-x64-shared-MT-Release", "dc703575a68594435f53dec93df775ff4913af851a196c0537163f21d8d2364e")),
        ("linux", "x86_64") => Some(("sherpa-onnx-v2.0.1-linux-x64-shared", "2e823376987c3b6960d50726b6560155e2ad92da0b8b3886325773225ec6bdec")),
        ("macos", _) => Some(("sherpa-onnx-v2.0.1-osx-universal2-shared", "bf6718f708e15390f1da133dd888661a68df270791b178aaf765d4e3f0baa9fe")),
        _ => None,
    }
}

pub struct Voice {
    pub id: &'static str,
    /// Speaker id in the v1.0 voices.bin
    sid: u32,
    pub note: &'static str,
}

pub const VOICES: &[Voice] = &[
    Voice { id: "af_heart", sid: 3, note: "US English, warm and expressive (recommended)" },
    Voice { id: "af_bella", sid: 2, note: "US English, lively and bright" },
    Voice { id: "af_nicole", sid: 6, note: "US English, soft and close, almost a whisper" },
    Voice { id: "af_sky", sid: 10, note: "US English, light and young" },
    Voice { id: "af_nova", sid: 7, note: "US English, clear and friendly" },
    Voice { id: "af_sarah", sid: 9, note: "US English, calm" },
    Voice { id: "bf_emma", sid: 21, note: "British English, gentle" },
    Voice { id: "bf_isabella", sid: 22, note: "British English, bright" },
    Voice { id: "am_michael", sid: 16, note: "US English, male" },
    Voice { id: "bm_george", sid: 26, note: "British English, male" },
];

pub fn is_voice(id: &str) -> bool {
    VOICES.iter().any(|v| v.id == id)
}

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("kokoro");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn engine_exe(app: &AppHandle) -> Option<PathBuf> {
    let (asset, _) = engine_asset()?;
    let name = if cfg!(windows) { "sherpa-onnx-offline-tts.exe" } else { "sherpa-onnx-offline-tts" };
    let exe = root(app).ok()?.join("engine").join(asset).join("bin").join(name);
    exe.is_file().then_some(exe)
}

fn model_dir(app: &AppHandle) -> Option<PathBuf> {
    let dir = root(app).ok()?.join(MODEL);
    MODEL_FILES.iter().all(|f| dir.join(f).is_file()).then_some(dir)
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
    /// sherpa-onnx has a build for this platform
    supported: bool,
    /// Engine and model are both downloaded
    ready: bool,
    size_mb: u32,
    voices: Vec<VoiceInfo>,
}

pub fn status(app: &AppHandle) -> Status {
    Status {
        supported: engine_asset().is_some(),
        ready: engine_exe(app).is_some() && model_dir(app).is_some(),
        size_mb: 160,
        voices: VOICES.iter().map(|v| VoiceInfo { id: v.id, note: v.note }).collect(),
    }
}

/// Downloads whatever is missing: the engine, then the model.
pub async fn download(app: &AppHandle) -> Result<(), String> {
    let (asset, sha) = engine_asset().ok_or("Kokoro has no build for this computer")?;
    let root = root(app)?;

    if engine_exe(app).is_none() {
        let archive = root.join(format!("{asset}.tar.bz2"));
        let url = format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/{ENGINE_RELEASE}/{asset}.tar.bz2");
        crate::tts::download(app, &url, &archive, Some(sha), "kokoro").await?;
        let engine = root.join("engine");
        let unpacked = crate::tts::unpack(&archive, &engine).await;
        tokio::fs::remove_file(&archive).await.ok();
        if unpacked.is_err() || engine_exe(app).is_none() {
            return Err("Couldn't unpack the Kokoro engine".into());
        }
        prune_engine(&engine.join(asset)).await;
        log::info!("sherpa-onnx {ENGINE_RELEASE} installed");
    }

    if model_dir(app).is_none() {
        let archive = root.join(format!("{MODEL}.tar.bz2"));
        let url = format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/{MODEL}.tar.bz2");
        crate::tts::download(app, &url, &archive, Some(MODEL_SHA256), "kokoro").await?;
        // Unpack beside the final folder, then keep only what we use
        let tmp = root.join("model-unpack");
        let unpacked = crate::tts::unpack(&archive, &tmp).await;
        tokio::fs::remove_file(&archive).await.ok();
        let result = match unpacked {
            Ok(()) => keep_model_files(&tmp.join(MODEL), &root.join(MODEL)).await,
            Err(e) => Err(e),
        };
        tokio::fs::remove_dir_all(&tmp).await.ok();
        if result.is_err() || model_dir(app).is_none() {
            return Err("Couldn't unpack the Kokoro voices".into());
        }
        log::info!("Kokoro model {MODEL} installed");
    }
    Ok(())
}

/// The release has ~20 other tools and the C/C++ headers; only the TTS tool is used.
async fn prune_engine(dir: &Path) {
    tokio::fs::remove_dir_all(dir.join("include")).await.ok();
    // On Windows the DLLs are in bin/ too; elsewhere the tool loads them from lib/
    if cfg!(windows) {
        tokio::fs::remove_dir_all(dir.join("lib")).await.ok();
    }
    if let Ok(mut entries) = tokio::fs::read_dir(dir.join("bin")).await {
        while let Ok(Some(e)) = entries.next_entry().await {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("sherpa-onnx") && !name.starts_with("sherpa-onnx-offline-tts") {
                tokio::fs::remove_file(e.path()).await.ok();
            }
        }
    }
}

async fn keep_model_files(from: &Path, to: &Path) -> Result<(), String> {
    tokio::fs::remove_dir_all(to).await.ok();
    tokio::fs::create_dir_all(to).await.map_err(|e| e.to_string())?;
    for f in MODEL_FILES.iter().chain(&["LICENSE"]) {
        let src = from.join(f);
        if src.is_file() {
            tokio::fs::rename(&src, to.join(f)).await.map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub async fn delete(app: &AppHandle) -> Result<(), String> {
    tokio::fs::remove_dir_all(root(app)?).await.map_err(|e| e.to_string())
}

/// The text goes on the command line, which the tool reads in the ANSI code
/// page on Windows, so curly quotes, dashes and accents are made plain ASCII.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in crate::tts::speakable(text).chars() {
        match c {
            '‘' | '’' | 'ʼ' | '′' => out.push('\''),
            '“' | '”' | '″' => out.push('"'),
            '–' | '—' => out.push(','),
            '…' => out.push_str("..."),
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => out.push('a'),
            'è' | 'é' | 'ê' | 'ë' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' => out.push('i'),
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' => out.push('o'),
            'ù' | 'ú' | 'û' | 'ü' => out.push('u'),
            'ñ' => out.push('n'),
            'ç' => out.push('c'),
            c if c.is_ascii() => out.push(c),
            _ => {}
        }
    }
    // A leading "--" would be read as an option
    out.trim().trim_start_matches('-').trim().to_string()
}

/// Speaks `text` with `voice_id` at `rate` (1.0 normal) and returns a WAV file.
pub async fn synthesize(app: &AppHandle, text: &str, voice_id: &str, rate: f32) -> Result<Vec<u8>, String> {
    let exe = engine_exe(app).ok_or("Kokoro isn't downloaded yet (Voice tab)")?;
    let dir = model_dir(app).ok_or("Kokoro isn't downloaded yet (Voice tab)")?;
    let voice = VOICES.iter().find(|v| v.id == voice_id).unwrap_or(&VOICES[0]);
    let text = plain(text);
    if !text.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err("Nothing to say".into());
    }
    // British voices read with the British lexicon
    let lexicon = if voice.id.starts_with('b') { "lexicon-gb-en.txt" } else { "lexicon-us-en.txt" };
    let threads = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4));

    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("diy-waifu-kokoro-{}-{n}.wav", std::process::id()));

    // Model paths are relative to the model folder, so a non-ASCII user name
    // can't break them the way it could in the ANSI command line
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.current_dir(&dir)
        .arg("--debug=0")
        .arg("--kokoro-model=model.int8.onnx")
        .arg("--kokoro-voices=voices.bin")
        .arg("--kokoro-tokens=tokens.txt")
        .arg(format!("--kokoro-lexicon={lexicon}"))
        .arg(format!("--num-threads={threads}"))
        .arg(format!("--sid={}", voice.sid))
        .arg(format!("--speed={:.2}", rate.clamp(0.5, 2.0)))
        .arg("--output-filename")
        .arg(&out)
        .arg(&text)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash
    let child = cmd.spawn().map_err(|e| format!("Couldn't start Kokoro: {e}"))?;
    let result = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| "Kokoro took too long".to_string())?
        .map_err(|e| e.to_string())?;
    let wav = tokio::fs::read(&out).await;
    tokio::fs::remove_file(&out).await.ok();
    if !result.status.success() {
        let err = String::from_utf8_lossy(&result.stderr);
        log::warn!("Kokoro failed: {}", err.trim());
        return Err("Kokoro couldn't speak that".into());
    }
    wav.map_err(|e| format!("Kokoro made no audio: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_text_plain_for_the_command_line() {
        assert_eq!(plain("I’m “so” happy — really… 😊"), "I'm \"so\" happy , really...");
        assert_eq!(plain("--verbose café"), "verbose cafe");
        assert_eq!(plain("नमस्ते"), "");
    }

    #[test]
    fn voices_are_unique_and_in_range() {
        let mut ids: Vec<_> = VOICES.iter().map(|v| v.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), VOICES.len());
        assert!(VOICES.iter().all(|v| v.sid <= 52));
    }
}
