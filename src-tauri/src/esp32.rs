//! What the ESP32 body needs from the PC, served by the page server on :8081:
//!
//! - `/esp32/manifest.json`: the avatar manifest (gifs/manifest.json) plus, per
//!   file, its size and an id that changes when the art does. The ESP32 caches
//!   GIFs in its flash by id, so new art shows up without reflashing.
//! - `/esp32/gifs/<name>`: that GIF resized to fit the 200x240 GIF area and
//!   flattened onto black (AnimatedGIF draws at native size, and transparency
//!   costs it time). Converted once per run and kept in memory.
//!
//! Also advertises the app over mDNS as `_companion._tcp` (host `diy-waifu.local`),
//! so the ESP32 finds the PC without a typed-in address.

use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use image::{AnimationDecoder, Delay, Frame, Rgba, RgbaImage};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock};

pub const MAX_W: u32 = 200;
pub const MAX_H: u32 = 240;
/// Frames beyond this are dropped; the ESP32's flash holds about 900 KB of GIFs
const MAX_FRAMES: usize = 120;

type Cache = Mutex<HashMap<String, Arc<Vec<u8>>>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Resizes and flattens an animated GIF for the ESP32 screen.
pub fn convert(src: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = GifDecoder::new(Cursor::new(src)).map_err(|e| e.to_string())?;
    // into_frames applies each frame's disposal, so every frame is a full picture
    let frames = decoder.into_frames().collect_frames().map_err(|e| e.to_string())?;
    let first = frames.first().ok_or("GIF has no frames")?;
    let (w, h) = first.buffer().dimensions();
    let scale = (MAX_W as f32 / w as f32).min(MAX_H as f32 / h as f32).min(1.0);
    let (nw, nh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));

    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new_with_speed(&mut out, 10);
        enc.set_repeat(Repeat::Infinite).map_err(|e| e.to_string())?;
        for f in frames.iter().take(MAX_FRAMES) {
            let resized = image::imageops::resize(f.buffer(), nw, nh, image::imageops::FilterType::Triangle);
            let mut flat = RgbaImage::from_pixel(nw, nh, Rgba([0, 0, 0, 255]));
            image::imageops::overlay(&mut flat, &resized, 0, 0);
            // Fully opaque, so the encoder writes no transparent index
            for p in flat.pixels_mut() {
                p.0[3] = 255;
            }
            let (num, den) = f.delay().numer_denom_ms();
            let delay = Delay::from_numer_denom_ms(num.max(20 * den), den);
            enc.encode_frame(Frame::from_parts(flat, 0, 0, delay)).map_err(|e| e.to_string())?;
        }
    }
    Ok(out)
}

/// The converted GIF `name` from the repo's gifs/ folder, converting it on first use.
pub async fn gif(name: &str) -> Option<Arc<Vec<u8>>> {
    if let Some(hit) = cache().lock().unwrap().get(name) {
        return Some(hit.clone());
    }
    let src = crate::phone::gif_bytes(name)?;
    let owned = name.to_string();
    let converted = tokio::task::spawn_blocking(move || convert(src))
        .await
        .ok()?
        .map_err(|e| log::warn!("Couldn't convert {owned} for the ESP32: {e}"))
        .ok()?;
    let converted = Arc::new(converted);
    cache().lock().unwrap().insert(name.to_string(), converted.clone());
    Some(converted)
}

fn short_id(data: &[u8]) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut h);
    format!("{:08x}", h.finish() as u32)
}

/// gifs/manifest.json plus {"files": {name: {"size", "id"}}} for the converted GIFs.
pub async fn manifest() -> serde_json::Value {
    let mut manifest: serde_json::Value =
        serde_json::from_str(crate::phone::gif_manifest()).unwrap_or_else(|_| serde_json::json!({}));
    let mut names: Vec<String> = manifest["states"]
        .as_object()
        .map(|o| o.values().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if let Some(d) = manifest["default"].as_str() {
        names.push(d.to_string());
    }
    names.sort();
    names.dedup();
    let mut files = serde_json::Map::new();
    for n in names {
        if let Some(data) = gif(&n).await {
            files.insert(n, serde_json::json!({ "size": data.len(), "id": short_id(&data) }));
        }
    }
    manifest["files"] = serde_json::Value::Object(files);
    manifest
}

/// Keeps the mDNS advertisement alive; dropping it stops advertising.
pub struct Advertiser(#[allow(dead_code)] Option<mdns_sd::ServiceDaemon>);

pub fn advertise(ws_port: u16, http_port: u16) -> Advertiser {
    let run = || -> Result<mdns_sd::ServiceDaemon, String> {
        let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| e.to_string())?;
        let ip = crate::phone::lan_ip().ok_or("not on a network")?;
        let props = [("http", http_port.to_string()), ("app", "diy-waifu".to_string())];
        let info = mdns_sd::ServiceInfo::new(
            "_companion._tcp.local.",
            "diy-waifu",
            "diy-waifu.local.",
            ip,
            ws_port,
            &props[..],
        )
        .map_err(|e| e.to_string())?
        .enable_addr_auto();
        daemon.register(info).map_err(|e| e.to_string())?;
        log::info!("Advertising diy-waifu.local ({ip}) over mDNS");
        Ok(daemon)
    };
    match run() {
        Ok(d) => Advertiser(Some(d)),
        Err(e) => {
            log::warn!("mDNS advertising is off: {e}");
            Advertiser(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_repo_gif_to_fit_the_screen() {
        let src = crate::phone::gif_bytes("mai-san-wave.gif").unwrap();
        let out = convert(src).unwrap();
        let frames = GifDecoder::new(Cursor::new(&out)).unwrap().into_frames().collect_frames().unwrap();
        let (w, h) = frames[0].buffer().dimensions();
        assert!(w <= MAX_W && h <= MAX_H, "{w}x{h}");
        assert!(frames.len() > 1);
        assert!(out.len() < 900_000, "{} bytes", out.len());
    }
}
