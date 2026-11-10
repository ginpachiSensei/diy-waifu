//! Natural offline voices with Piper (https://github.com/rhasspy/piper).
//!
//! The engine (~22 MB) and voices (~63 MB each) are downloaded from the Voice
//! tab into `<app data>/piper/`, pinned and SHA-256 checked. `synthesize` runs
//! `piper --model <voice> --output_file <wav>` with the text on stdin and
//! returns the WAV; the pet plays it (settings.tts_engine == "piper").
//! Windows voices (speechSynthesis in the webview) stay the default.

use futures::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

const ENGINE_RELEASE: &str = "2023.11.14-2";
const VOICES_REV: &str = "v1.0.0";

/// (release asset, sha256) for this platform
fn engine_asset() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some(("piper_windows_amd64.zip", "f3c58906402b24f3a96d92145f58acba6d86c9b5db896d207f78dc80811efcea")),
        ("linux", "x86_64") => Some(("piper_linux_x86_64.tar.gz", "a50cb45f355b7af1f6d758c1b360717877ba0a398cc8cbe6d2a7a3a26e225992")),
        ("linux", "aarch64") => Some(("piper_linux_aarch64.tar.gz", "fea0fd2d87c54dbc7078d0f878289f404bd4d6eea6e7444a77835d1537ab88eb")),
        ("macos", "aarch64") => Some(("piper_macos_aarch64.tar.gz", "6b1eb03b3735946cb35216e063e7eebcc33a6bbf5dd96ec0217959bf1cdcb0cc")),
        ("macos", "x86_64") => Some(("piper_macos_x64.tar.gz", "ced85c0a3df13945b1e623b878a48fdc2854d5c485b4b67f62857cf551deaf8b")),
        _ => None,
    }
}

pub struct Voice {
    pub id: &'static str,
    /// Folder in the rhasspy/piper-voices repo
    path: &'static str,
    sha256: &'static str,
    pub size_mb: u32,
    pub note: &'static str,
}

pub const VOICES: &[Voice] = &[
    Voice { id: "en_US-amy-medium", path: "en/en_US/amy/medium", sha256: "b3a6e47b57b8c7fbe6a0ce2518161a50f59a9cdd8a50835c02cb02bdd6206c18", size_mb: 63, note: "US English, bright female voice (recommended)" },
    Voice { id: "en_US-hfc_female-medium", path: "en/en_US/hfc_female/medium", sha256: "914c473788fc1fa8b63ace1cdcdb44588f4ae523d3ab37df1536616835a140b7", size_mb: 63, note: "US English, softer female voice" },
    Voice { id: "en_US-kristin-medium", path: "en/en_US/kristin/medium", sha256: "5849957f929cbf720c258f8458692d6103fff2f0e3d3b19c8259474bb06a18d4", size_mb: 64, note: "US English, calm female voice" },
    Voice { id: "en_US-lessac-medium", path: "en/en_US/lessac/medium", sha256: "5efe09e69902187827af646e1a6e9d269dee769f9877d17b16b1b46eeaaf019f", size_mb: 63, note: "US English, clear neutral voice" },
    Voice { id: "en_GB-alba-medium", path: "en/en_GB/alba/medium", sha256: "401369c4a81d09fdd86c32c5c864440811dbdcc66466cde2d64f7133a66ad03b", size_mb: 63, note: "British English, female" },
    Voice { id: "hi_IN-pratham-medium", path: "hi/hi_IN/pratham/medium", sha256: "169964b0871667f6793416d4b35e97357a68ba1ad01df8580c28048989ee7693", size_mb: 64, note: "Hindi, male" },
];

fn voice(id: &str) -> Option<&'static Voice> {
    VOICES.iter().find(|v| v.id == id)
}

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("piper");
    std::fs::create_dir_all(dir.join("voices")).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn engine_exe(app: &AppHandle) -> Option<PathBuf> {
    let exe = root(app).ok()?.join("engine").join("piper").join(if cfg!(windows) { "piper.exe" } else { "piper" });
    exe.is_file().then_some(exe)
}

fn voice_file(app: &AppHandle, id: &str) -> Option<PathBuf> {
    let p = root(app).ok()?.join("voices").join(format!("{id}.onnx"));
    (p.is_file() && p.with_extension("onnx.json").is_file()).then_some(p)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceInfo {
    id: &'static str,
    size_mb: u32,
    note: &'static str,
    downloaded: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Piper has a build for this platform
    supported: bool,
    engine_ready: bool,
    voices: Vec<VoiceInfo>,
}

pub fn status(app: &AppHandle) -> Status {
    Status {
        supported: engine_asset().is_some(),
        engine_ready: engine_exe(app).is_some(),
        voices: VOICES
            .iter()
            .map(|v| VoiceInfo { id: v.id, size_mb: v.size_mb, note: v.note, downloaded: voice_file(app, v.id).is_some() })
            .collect(),
    }
}

/// Downloads `url` to `dest`, emitting `voice:progress` under `progress_id`.
/// With `sha256`, the file must match or it's thrown away.
pub(crate) async fn download(app: &AppHandle, url: &str, dest: &Path, sha256: Option<&str>, progress_id: &str) -> Result<(), String> {
    let part = dest.with_extension("part");
    let resp = reqwest::Client::new()
        .get(url)
        .header("User-Agent", crate::tools::USER_AGENT)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Download failed: {}", resp.status()));
    }
    let total = resp.content_length();
    let mut out = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    let (mut done, mut last_emit) = (0u64, Instant::now());
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download failed: {e}"))?;
        hasher.update(&chunk);
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        if last_emit.elapsed() > Duration::from_millis(250) {
            last_emit = Instant::now();
            app.emit("voice:progress", serde_json::json!({ "model": progress_id, "completed": done, "total": total })).ok();
        }
    }
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    if let Some(expected) = sha256 {
        let got = format!("{:x}", hasher.finalize());
        if got != expected {
            tokio::fs::remove_file(&part).await.ok();
            return Err("The download was corrupted (checksum mismatch). Try again.".into());
        }
    }
    tokio::fs::rename(&part, dest).await.map_err(|e| e.to_string())
}

pub async fn download_engine(app: &AppHandle) -> Result<(), String> {
    let (asset, sha) = engine_asset().ok_or("Piper has no build for this computer")?;
    let root = root(app)?;
    let archive = root.join(asset);
    let url = format!("https://github.com/rhasspy/piper/releases/download/{ENGINE_RELEASE}/{asset}");
    download(app, &url, &archive, Some(sha), "piper").await?;

    let engine = root.join("engine");
    let unpacked = unpack(&archive, &engine).await;
    tokio::fs::remove_file(&archive).await.ok();
    if unpacked.is_err() || engine_exe(app).is_none() {
        return Err("Couldn't unpack Piper".into());
    }
    log::info!("Piper {ENGINE_RELEASE} installed");
    Ok(())
}

/// Unpacks a zip / .tar.gz / .tar.bz2 into a fresh `dest` folder.
pub(crate) async fn unpack(archive: &Path, dest: &Path) -> Result<(), String> {
    tokio::fs::remove_dir_all(dest).await.ok();
    tokio::fs::create_dir_all(dest).await.map_err(|e| e.to_string())?;
    // bsdtar ships with Windows 10+ and reads zips and bzip2; Git Bash's tar would read "C:" as a host
    let tar = if cfg!(windows) { r"C:\Windows\System32\tar.exe" } else { "tar" };
    let status = tokio::process::Command::new(tar)
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .await
        .map_err(|e| format!("Couldn't unpack: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Couldn't unpack".into())
    }
}

pub async fn download_voice(app: &AppHandle, id: &str) -> Result<(), String> {
    let v = voice(id).ok_or("Unknown voice")?;
    let dir = root(app)?.join("voices");
    let base = format!("https://huggingface.co/rhasspy/piper-voices/resolve/{VOICES_REV}/{}/{}", v.path, v.id);
    // The small config first, so a finished .onnx always has its .json
    download(app, &format!("{base}.onnx.json"), &dir.join(format!("{id}.onnx.json")), None, id).await?;
    download(app, &format!("{base}.onnx"), &dir.join(format!("{id}.onnx")), Some(v.sha256), id).await?;
    log::info!("Downloaded Piper voice {id}");
    Ok(())
}

pub async fn delete_voice(app: &AppHandle, id: &str) -> Result<(), String> {
    voice(id).ok_or("Unknown voice")?;
    let dir = root(app)?.join("voices");
    for ext in ["onnx", "onnx.json"] {
        tokio::fs::remove_file(dir.join(format!("{id}.{ext}"))).await.ok();
    }
    Ok(())
}

/// Text as Piper should hear it: one line (each line is a separate utterance),
/// without emoji or markdown symbols.
pub(crate) fn speakable(text: &str) -> String {
    let emoji = |c: char| {
        matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0xFE00..=0xFE0F | 0x200D | 0x20E3)
    };
    let cleaned: String = text
        .chars()
        .filter(|c| !emoji(*c) && !matches!(c, '*' | '_' | '`' | '#' | '>' | '~'))
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Speaks `text` with `voice_id` at `rate` (1.0 normal) and returns a WAV file.
pub async fn synthesize(app: &AppHandle, text: &str, voice_id: &str, rate: f32) -> Result<Vec<u8>, String> {
    let exe = engine_exe(app).ok_or("Piper isn't downloaded yet (Voice tab)")?;
    let model = voice_file(app, voice_id).ok_or("That Piper voice isn't downloaded yet (Voice tab)")?;
    let text = speakable(text);
    if text.is_empty() {
        return Err("Nothing to say".into());
    }
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("diy-waifu-tts-{}-{n}.wav", std::process::id()));
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("--model")
        .arg(&model)
        .arg("--output_file")
        .arg(&out)
        .arg("--length_scale")
        .arg(format!("{:.2}", 1.0 / rate.clamp(0.5, 2.0)))
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start Piper: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).await.map_err(|e| e.to_string())?;
        stdin.write_all(b"\n").await.ok();
    }
    let result = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| "Piper took too long".to_string())?
        .map_err(|e| e.to_string())?;
    let wav = tokio::fs::read(&out).await;
    tokio::fs::remove_file(&out).await.ok();
    if !result.status.success() {
        let err = String::from_utf8_lossy(&result.stderr);
        log::warn!("Piper failed: {}", err.trim());
        return Err("Piper couldn't speak that".into());
    }
    wav.map_err(|e| format!("Piper made no audio: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_text_for_speaking() {
        assert_eq!(speakable("Hi! 😊 I'm **here**\nfor you…"), "Hi! I'm here for you…");
        assert_eq!(speakable("नमस्ते दोस्त"), "नमस्ते दोस्त");
        assert_eq!(speakable("🎉🎉"), "");
    }

    #[test]
    fn every_voice_is_pinned() {
        for v in VOICES {
            assert_eq!(v.sha256.len(), 64, "{}", v.id);
            assert!(v.path.ends_with("/medium"));
        }
    }
}
