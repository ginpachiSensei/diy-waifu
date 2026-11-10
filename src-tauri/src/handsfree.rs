//! Hands-free talking: say the wake phrase ("hey <companion name>" by default,
//! or settings.wake_word), then what you want.
//!
//! With settings.hands_free on, a thread keeps the microphone open and runs a
//! small voice-activity detector: an energy threshold that follows the room's
//! noise floor. Each stretch of speech (with a little audio from before it) is
//! sent to the local whisper server. Only speech that starts with the wake
//! phrase is used; everything else is dropped without being logged or stored.
//! Saying just the wake phrase makes it listen for the next sentence (8s).
//!
//! It stops while paused, and whenever voice input or the setting is off.
//! Audio never leaves the PC.

use crate::db::Db;
use crate::settings::Settings;
use crate::voice::{self, Captured};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

const RATE: usize = 16_000;
/// 30 ms frames
const FRAME: usize = RATE * 30 / 1000;
/// Speech must last this many frames to start an utterance (90 ms)
const START_FRAMES: usize = 3;
/// This much silence ends it (800 ms)
const END_FRAMES: usize = 27;
/// Audio kept from just before speech started (300 ms)
const PRE_ROLL: usize = RATE * 300 / 1000;
const MIN_UTTERANCE: usize = RATE / 2;
const MAX_UTTERANCE: usize = RATE * 15;
/// After only the wake phrase, how long to wait for the request
const FOLLOW_UP: Duration = Duration::from_secs(8);

/// Finds utterances in a 16 kHz mono stream, frame by frame.
pub struct Vad {
    noise: f32,
    loud: usize,
    quiet: usize,
    speaking: bool,
    pre: Vec<f32>,
    current: Vec<f32>,
    pending: Vec<f32>,
}

impl Default for Vad {
    fn default() -> Self {
        Self { noise: 0.005, loud: 0, quiet: 0, speaking: false, pre: Vec::new(), current: Vec::new(), pending: Vec::new() }
    }
}

impl Vad {
    /// Feeds samples; returns any utterances that just ended.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let mut done = Vec::new();
        while self.pending.len() >= FRAME {
            let frame: Vec<f32> = self.pending.drain(..FRAME).collect();
            if let Some(u) = self.frame(&frame) {
                done.push(u);
            }
        }
        done
    }

    fn frame(&mut self, frame: &[f32]) -> Option<Vec<f32>> {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
        let threshold = (self.noise * 3.0).max(0.01);
        let loud = rms > threshold;
        if !self.speaking {
            // Follow the room's noise level while nobody is talking
            self.noise = if rms < self.noise { 0.9 * self.noise + 0.1 * rms } else { 0.995 * self.noise + 0.005 * rms };
            self.loud = if loud { self.loud + 1 } else { 0 };
            self.pre.extend_from_slice(frame);
            if self.loud >= START_FRAMES {
                self.speaking = true;
                self.quiet = 0;
                self.current = std::mem::take(&mut self.pre);
            } else if self.pre.len() > PRE_ROLL {
                let extra = self.pre.len() - PRE_ROLL;
                self.pre.drain(..extra);
            }
            return None;
        }
        self.current.extend_from_slice(frame);
        self.quiet = if loud { 0 } else { self.quiet + 1 };
        if self.quiet >= END_FRAMES || self.current.len() >= MAX_UTTERANCE {
            self.speaking = false;
            self.loud = 0;
            let u = std::mem::take(&mut self.current);
            return (u.len() >= MIN_UTTERANCE).then_some(u);
        }
        None
    }
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Levenshtein distance, for names whisper spells slightly differently
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn close(heard: &str, want: &str) -> bool {
    heard == want || (want.chars().count() >= 3 && distance(heard, want) <= 1)
}

/// If `text` starts with the wake phrase, what came after it (maybe empty).
/// The leading "hey"/"hi"/"ok" is optional, since whisper often drops or changes it.
pub fn after_wake(text: &str, wake: &str) -> Option<String> {
    const GREETINGS: &[&str] = &["hey", "hi", "hay", "a", "ok", "okay", "hello", "yo"];
    let heard = words(text);
    let mut want = words(wake);
    if want.len() > 1 && GREETINGS.contains(&want[0].as_str()) {
        want.remove(0);
    }
    if want.is_empty() {
        return None;
    }
    // Skip up to two greeting words before the name
    let skip = heard.iter().take(2).take_while(|w| GREETINGS.contains(&w.as_str())).count();
    let rest = &heard[skip..];
    if rest.len() < want.len() || !rest.iter().zip(&want).all(|(h, w)| close(h, w)) {
        return None;
    }
    // Keep the original wording (punctuation, case) of what followed
    let mut seen = 0;
    let needed = skip + want.len();
    let mut cut = text.len();
    let mut in_word = false;
    for (i, c) in text.char_indices() {
        let is_word = c.is_alphanumeric() || c == '\'';
        if is_word && !in_word {
            if seen == needed {
                cut = i;
                break;
            }
            seen += 1;
        }
        in_word = is_word;
    }
    Some(text[cut..].trim_start_matches(|c: char| !c.is_alphanumeric()).trim().to_string())
}

pub fn wake_phrase(s: &Settings) -> String {
    if s.wake_word.trim().is_empty() {
        format!("hey {}", s.companion_name)
    } else {
        s.wake_word.trim().to_string()
    }
}

/// The capture thread, running while hands-free is on
struct Running {
    stop: Arc<AtomicBool>,
}

fn start_capture(tx: mpsc::UnboundedSender<Vec<f32>>) -> Result<Running, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (stream, buf, rate, channels) = match voice::open_mic() {
            Ok(v) => {
                ready_tx.send(Ok(())).ok();
                v
            }
            Err(e) => {
                ready_tx.send(Err(e)).ok();
                return;
            }
        };
        let mut vad = Vad::default();
        while !flag.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(60));
            let samples = std::mem::take(&mut *buf.lock().unwrap());
            if samples.is_empty() {
                continue;
            }
            let mono = voice::to_16k_mono(&Captured { samples, rate, channels });
            for u in vad.push(&mono) {
                if tx.send(u).is_err() {
                    return;
                }
            }
        }
        drop(stream);
    });
    match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Ok(Running { stop }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("The microphone didn't start".into()),
    }
}

fn wanted(app: &AppHandle) -> bool {
    let s = Settings::load(&app.state::<Db>());
    s.hands_free
        && s.voice_input
        && voice::model_path(app, &s.whisper_model).is_some()
        && !app.state::<crate::AppState>().paused.load(Ordering::Relaxed)
}

/// Whether hands-free listening is running right now (for the Voice tab)
pub fn is_on(app: &AppHandle) -> bool {
    app.state::<HandsFree>().0.load(Ordering::Relaxed)
}

#[derive(Default)]
pub struct HandsFree(AtomicBool);

/// Starts and stops listening as settings and pause change, and handles what it hears.
pub async fn run(app: AppHandle) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<f32>>();
    let mut running: Option<Running> = None;
    let mut awaiting_request: Option<Instant> = None;
    let mut check = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = check.tick() => {
                let want = wanted(&app);
                if want && running.is_none() {
                    match start_capture(tx.clone()) {
                        Ok(r) => {
                            log::info!("Hands-free listening on");
                            running = Some(r);
                        }
                        Err(e) => {
                            log::warn!("Hands-free couldn't start: {e}");
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                    }
                } else if !want {
                    if let Some(r) = running.take() {
                        r.stop.store(true, Ordering::Relaxed);
                        log::info!("Hands-free listening off");
                    }
                }
                let on = running.is_some();
                if app.state::<HandsFree>().0.swap(on, Ordering::Relaxed) != on {
                    app.emit("voice:hands_free", on).ok();
                }
            }
            Some(audio) = rx.recv() => {
                // Push-to-talk is in charge while it's held
                if running.is_none() || voice::is_recording(&app) {
                    continue;
                }
                let text = match voice::transcribe(&app, &audio).await {
                    Ok(t) if !voice::is_noise(&t) => t,
                    Ok(_) => continue,
                    Err(e) => {
                        log::warn!("Hands-free transcription failed: {e}");
                        continue;
                    }
                };
                let wake = wake_phrase(&Settings::load(&app.state::<Db>()));
                let request = match after_wake(&text, &wake) {
                    Some(rest) if !rest.is_empty() => rest,
                    Some(_) => {
                        // Just the name: listen for the request next
                        awaiting_request = Some(Instant::now());
                        voice::set_state(&app, "listening", None);
                        continue;
                    }
                    None if awaiting_request.is_some_and(|t| t.elapsed() < FOLLOW_UP) => text,
                    None => continue, // not for us; dropped unlogged
                };
                awaiting_request = None;
                voice::set_state(&app, "idle", None);
                log::info!("Heard (hands-free): {request}");
                app.emit("voice:transcript", &request).ok();
                let _ = crate::companion::handle_user_message(&app, "voice", &request).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_wake_phrase() {
        let w = "hey Mai";
        assert_eq!(after_wake("Hey Mai, what's the weather?", w).as_deref(), Some("what's the weather?"));
        assert_eq!(after_wake("Hi, May. Remind me to stretch.", w).as_deref(), Some("Remind me to stretch."));
        assert_eq!(after_wake("Mai!", w).as_deref(), Some(""));
        assert_eq!(after_wake("I told my friend about it", w), None);
        assert_eq!(after_wake("Hey Maya what's up", "hey Mayu").as_deref(), Some("what's up"));
        assert_eq!(after_wake("Okay computer, play music", "ok computer").as_deref(), Some("play music"));
        assert_eq!(after_wake("hey computer", "ok computer").as_deref(), Some(""));
    }

    fn tone(len: usize, amp: f32) -> Vec<f32> {
        (0..len).map(|i| amp * (i as f32 * 0.3).sin()).collect()
    }

    #[test]
    fn vad_cuts_utterances_out_of_noise() {
        let mut vad = Vad::default();
        let mut audio = tone(RATE, 0.002); // 1s room noise
        audio.extend(tone(RATE, 0.3)); // 1s speech
        audio.extend(tone(RATE, 0.002)); // 1s quiet
        let found = vad.push(&audio);
        assert_eq!(found.len(), 1);
        let secs = found[0].len() as f32 / RATE as f32;
        assert!((1.0..2.2).contains(&secs), "{secs}s");
        // A click is too short to count
        let mut click = tone(FRAME * 2, 0.5);
        click.extend(tone(RATE, 0.002));
        assert!(vad.push(&click).is_empty());
    }
}
