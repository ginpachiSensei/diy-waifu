//! Screen awareness. Every 2s, looks at the window in front (app + title) and
//! how long since the last keyboard/mouse input, then:
//! - sorts the app into a category with the rules in settings (coding, video, ...),
//! - hides anything on the privacy blocklist (it becomes "private", no title),
//! - tracks presence: active, idle (5 min), away (15 min; not while watching or gaming),
//! - records activity sessions (app + category + times, never titles),
//! - tells the bodies via {"type":"state","state":...} when the category or presence changes.
//!
//! Window titles stay in memory only. They reach the model only if
//! `settings.share_titles` is on.

use crate::db::{now_millis, Db};
use crate::settings::Settings;
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use sysinfo::{Pid, ProcessesToUpdate, System};
use tauri::{Emitter, Manager};

const POLL: Duration = Duration::from_secs(2);
const IDLE_AFTER_SECS: u64 = 5 * 60;
const AWAY_AFTER_SECS: u64 = 15 * 60;
/// Sessions are written to the DB at least this often, so a crash loses little
const SESSION_FLUSH_MS: i64 = 30_000;
const KEEP_SESSIONS_DAYS: i64 = 30;

/// Categories where no input is normal, so the user isn't marked away
const PASSIVE: &[&str] = &["video", "gaming"];

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// Matches a window. Empty fields match anything, but a rule with every field
/// empty matches nothing. All non-empty fields must match:
/// `app` is the exact process name, `title` and `path` are substrings.
/// Everything is compared lowercase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ActivityRule {
    pub category: String,
    pub app: String,
    pub title: String,
    pub path: String,
}

impl ActivityRule {
    fn new(category: &str, app: &str, title: &str, path: &str) -> Self {
        Self {
            category: category.into(),
            app: app.into(),
            title: title.into(),
            path: path.into(),
        }
    }

    pub fn matches(&self, w: &Window) -> bool {
        let (app, title, path) = (self.app.trim(), self.title.trim(), self.path.trim());
        if app.is_empty() && title.is_empty() && path.is_empty() {
            return false;
        }
        (app.is_empty() || w.app == app.to_lowercase())
            && (title.is_empty() || w.title.to_lowercase().contains(&title.to_lowercase()))
            && (path.is_empty() || w.path.to_lowercase().contains(&path.to_lowercase()))
    }
}

/// First match wins, so specific title rules come before whole-app rules
/// (a YouTube tab is "video" even though Chrome is "browsing").
pub fn default_rules() -> Vec<ActivityRule> {
    let r = ActivityRule::new;
    let mut rules = vec![];
    for t in ["youtube", "netflix", "prime video", "twitch", "hotstar", "crunchyroll", "disney+"] {
        rules.push(r("video", "", t, ""));
    }
    for a in ["vlc.exe", "mpc-hc64.exe", "potplayermini64.exe"] {
        rules.push(r("video", a, "", ""));
    }
    rules.push(r("music", "spotify.exe", "", ""));
    rules.push(r("music", "", "spotify", ""));
    rules.push(r("gaming", "", "", "steamapps\\common"));
    rules.push(r("gaming", "", "", "epic games"));
    for a in ["genshinimpact.exe", "yuanshen.exe", "valorant.exe", "cs2.exe", "javaw.exe", "minecraft.exe"] {
        rules.push(r("gaming", a, "", ""));
    }
    for a in [
        "code.exe",
        "cursor.exe",
        "devenv.exe",
        "idea64.exe",
        "pycharm64.exe",
        "rider64.exe",
        "sublime_text.exe",
        "windowsterminal.exe",
        "unity.exe",
    ] {
        rules.push(r("coding", a, "", ""));
    }
    for a in ["blender.exe", "photoshop.exe", "krita.exe", "aseprite.exe", "figma.exe", "clipstudiopaint.exe"] {
        rules.push(r("creative", a, "", ""));
    }
    for a in ["discord.exe", "whatsapp.exe", "telegram.exe", "slack.exe", "ms-teams.exe"] {
        rules.push(r("chatting", a, "", ""));
    }
    for t in ["whatsapp", "discord", "messenger"] {
        rules.push(r("chatting", "", t, ""));
    }
    for a in ["chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "opera.exe", "zen.exe"] {
        rules.push(r("browsing", a, "", ""));
    }
    rules
}

/// Windows that are never recorded or described, only shown as "private".
pub fn default_privacy_rules() -> Vec<ActivityRule> {
    let r = ActivityRule::new;
    let mut rules = vec![];
    for a in ["keepass.exe", "keepassxc.exe", "1password.exe", "bitwarden.exe"] {
        rules.push(r("private", a, "", ""));
    }
    for t in ["inprivate", "incognito", "private browsing"] {
        rules.push(r("private", "", t, ""));
    }
    rules
}

// ---------------------------------------------------------------------------
// What the OS tells us
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Window {
    pub pid: u32,
    /// Lowercase process name, e.g. "code.exe"
    pub app: String,
    pub title: String,
    pub path: String,
}

#[cfg(windows)]
fn foreground_window(sys: &mut System) -> Option<Window> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId};

    // SAFETY: plain Win32 queries on a window handle; the buffer outlives the call
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        let title = String::from_utf16_lossy(&buf[..len]);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let spid = Pid::from_u32(pid);
        sys.refresh_processes(ProcessesToUpdate::Some(&[spid]), true);
        let proc_ = sys.process(spid)?;
        Some(Window {
            pid,
            app: proc_.name().to_string_lossy().to_lowercase(),
            title,
            path: proc_
                .exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
    }
}

#[cfg(not(windows))]
fn foreground_window(_sys: &mut System) -> Option<Window> {
    None
}

/// Seconds since the last keyboard or mouse input.
#[cfg(windows)]
fn idle_secs() -> u64 {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: info is a properly sized LASTINPUTINFO
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return 0;
        }
        (GetTickCount().wrapping_sub(info.dwTime) / 1000) as u64
    }
}

#[cfg(not(windows))]
fn idle_secs() -> u64 {
    0
}

// ---------------------------------------------------------------------------
// The current activity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    /// False when screen awareness is off; everything else is then empty
    pub enabled: bool,
    pub app: Option<String>,
    /// Friendly name, e.g. "VS Code"
    pub label: Option<String>,
    pub category: Option<String>,
    /// None for private windows
    pub title: Option<String>,
    /// "active" | "idle" | "away"
    pub presence: String,
    /// When the current category started (unix millis)
    pub since: i64,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            enabled: false,
            app: None,
            label: None,
            category: None,
            title: None,
            presence: "active".into(),
            since: now_millis(),
        }
    }
}

pub fn friendly_name(app: &str) -> String {
    let base = app.strip_suffix(".exe").unwrap_or(app);
    let known = match base {
        "code" => "VS Code",
        "cursor" => "Cursor",
        "devenv" => "Visual Studio",
        "chrome" => "Chrome",
        "msedge" => "Edge",
        "firefox" => "Firefox",
        "brave" => "Brave",
        "windowsterminal" => "Terminal",
        "explorer" => "File Explorer",
        "unity" => "Unity",
        "blender" => "Blender",
        "spotify" => "Spotify",
        "discord" => "Discord",
        "whatsapp" => "WhatsApp",
        "javaw" => "Minecraft",
        "genshinimpact" | "yuanshen" => "Genshin Impact",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let mut chars = base.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => app.to_string(),
    }
}

/// Category and visible details for a window: (app, label, category, title).
fn classify(w: &Window, s: &Settings) -> (String, String, String, Option<String>) {
    if s.privacy_rules.iter().any(|r| r.matches(w)) {
        return ("private".into(), "a private window".into(), "private".into(), None);
    }
    let category = s
        .activity_rules
        .iter()
        .find(|r| r.matches(w))
        .map(|r| r.category.trim().to_lowercase())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "other".into());
    (w.app.clone(), friendly_name(&w.app), category, Some(w.title.clone()))
}

fn presence_for(idle: u64, category: Option<&str>) -> &'static str {
    if category.is_some_and(|c| PASSIVE.contains(&c)) {
        return "active";
    }
    if idle >= AWAY_AFTER_SECS {
        "away"
    } else if idle >= IDLE_AFTER_SECS {
        "idle"
    } else {
        "active"
    }
}

/// One line for the system prompt, or None if there's nothing to say.
pub fn prompt_context(a: &Activity, s: &Settings) -> Option<String> {
    if !a.enabled {
        return None;
    }
    let category = a.category.as_deref()?;
    if category == "private" {
        return None;
    }
    let label = a.label.as_deref()?;
    let mins = ((now_millis() - a.since) / 60_000).max(0);
    let user = if s.user_name.is_empty() { "the user" } else { s.user_name.as_str() };
    let mut line = if category == "other" {
        format!("Right now {user} is using {label}")
    } else {
        format!("Right now {user} is using {label} ({category})")
    };
    if mins >= 2 {
        line.push_str(&format!(", and has been {category} for about {mins} minutes"));
    }
    line.push('.');
    if s.share_titles {
        if let Some(t) = a.title.as_deref().filter(|t| !t.trim().is_empty()) {
            line.push_str(&format!(" The window title is \"{}\".", t.trim()));
        }
    }
    Some(line)
}

/// Messages a newly connected body needs to show the current state.
pub fn state_messages(a: &Activity) -> Vec<String> {
    let mut out = vec![];
    if let Some(c) = &a.category {
        out.push(serde_json::json!({ "type": "state", "state": c }).to_string());
    }
    if a.presence == "away" {
        out.push(serde_json::json!({ "type": "state", "state": "away" }).to_string());
    }
    out
}

fn broadcast_state(app: &tauri::AppHandle, state: &str) {
    let msg = serde_json::json!({ "type": "state", "state": state }).to_string();
    let _ = app.state::<AppState>().broadcast_tx.send(msg);
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// The heavy-app check lists every process, so it runs every this many ticks (~6s)
const HEAVY_EVERY: u32 = 3;

/// The first heavy app in `running` (process names), if any.
fn find_heavy(running: impl IntoIterator<Item = String>, heavy: &[String]) -> Option<&str> {
    running.into_iter().find_map(|name| {
        let name = name.to_lowercase();
        heavy.iter().find(|h| **h == name).map(String::as_str)
    })
}

/// Auto-pauses while a heavy app (settings.heavy_apps) runs, so the model's RAM
/// and CPU go to that app instead. Runs whether or not screen awareness is on.
fn check_heavy_apps(app: &tauri::AppHandle, sys: &mut System, settings: &Settings) {
    let reason = if settings.auto_pause && !settings.heavy_apps.is_empty() {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let names = sys.processes().values().map(|p| p.name().to_string_lossy().into_owned());
        find_heavy(names, &settings.heavy_apps).map(|a| format!("{a} is running"))
    } else {
        None
    };
    // No-op unless it changed
    crate::pause::set_auto(app, reason);
}

struct Session {
    id: i64,
    app: String,
    category: String,
    flushed_at: i64,
}

pub async fn run(app: tauri::AppHandle) {
    let own_pid = std::process::id();
    let mut sys = System::new();
    let mut heavy_sys = System::new();
    let mut tick: u32 = 0;
    let mut last_window: Option<Window> = None;
    let mut session: Option<Session> = None;
    let db = app.state::<Db>();

    if let Err(e) = db.prune_sessions(now_millis() - KEEP_SESSIONS_DAYS * 86_400_000) {
        log::warn!("Couldn't prune old activity: {e}");
    }

    loop {
        tokio::time::sleep(POLL).await;
        let settings = Settings::load(&db);
        if tick.is_multiple_of(HEAVY_EVERY) {
            check_heavy_apps(&app, &mut heavy_sys, &settings);
        }
        tick = tick.wrapping_add(1);
        let prev = app.state::<AppState>().activity.lock().unwrap().clone();
        let now = now_millis();

        let next = if !settings.screen_awareness {
            // Keep `since` stable so an unchanged "off" state isn't re-sent every tick
            Activity {
                since: if prev.enabled { now } else { prev.since },
                ..Activity::default()
            }
        } else {
            // Clicking the pet or the panel shouldn't count as switching apps
            if let Some(w) = foreground_window(&mut sys).filter(|w| w.pid != own_pid) {
                last_window = Some(w);
            }
            match &last_window {
                None => Activity {
                    enabled: true,
                    since: prev.since,
                    ..Activity::default()
                },
                Some(w) => {
                    let (app_name, label, category, title) = classify(w, &settings);
                    let presence = presence_for(idle_secs(), Some(&category));
                    let since = if prev.category.as_deref() == Some(category.as_str()) {
                        prev.since
                    } else {
                        now
                    };
                    Activity {
                        enabled: true,
                        app: Some(app_name),
                        label: Some(label),
                        category: Some(category),
                        title,
                        presence: presence.into(),
                        since,
                    }
                }
            }
        };

        // Sessions: one per (app, category) stretch, not while away or disabled
        let current = (next.enabled && next.presence != "away")
            .then(|| next.app.clone().zip(next.category.clone()))
            .flatten();
        let same = matches!((&session, &current), (Some(s), Some((a, c))) if s.app == *a && s.category == *c);
        if !same {
            if let Some(s) = session.take() {
                db.touch_session(s.id, now).ok();
            }
            if let Some((a, c)) = current {
                match db.start_session(&a, &c, now) {
                    Ok(id) => {
                        session = Some(Session {
                            id,
                            app: a,
                            category: c,
                            flushed_at: now,
                        })
                    }
                    Err(e) => log::warn!("Couldn't record activity: {e}"),
                }
            }
        } else if let Some(s) = session.as_mut() {
            if now - s.flushed_at >= SESSION_FLUSH_MS {
                db.touch_session(s.id, now).ok();
                s.flushed_at = now;
            }
        }

        if next == prev {
            continue;
        }
        *app.state::<AppState>().activity.lock().unwrap() = next.clone();
        app.emit("activity:changed", &next).ok();

        if next.category != prev.category {
            // "idle" for no category clears the activity on the bodies
            broadcast_state(&app, next.category.as_deref().unwrap_or("idle"));
        }
        let was_away = prev.presence == "away";
        let is_away = next.presence == "away";
        if was_away != is_away {
            broadcast_state(&app, if is_away { "away" } else { "active" });
        }
        if next.category != prev.category || was_away != is_away {
            log::info!(
                "Activity: {} ({}) {}",
                next.label.as_deref().unwrap_or("-"),
                next.category.as_deref().unwrap_or("-"),
                next.presence
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_heavy_process_names_case_insensitively() {
        let heavy = vec!["blender.exe".to_string()];
        let running = ["explorer.exe", "Blender.exe"].map(String::from);
        assert_eq!(find_heavy(running, &heavy), Some("blender.exe"));
        assert_eq!(find_heavy(["code.exe".to_string()], &heavy), None);
    }

    fn win(app: &str, title: &str, path: &str) -> Window {
        Window {
            pid: 1,
            app: app.into(),
            title: title.into(),
            path: path.into(),
        }
    }

    fn category(w: &Window) -> String {
        classify(w, &Settings::default()).2
    }

    #[test]
    fn default_rules_sort_common_apps() {
        assert_eq!(category(&win("code.exe", "main.rs - diy-waifu", "")), "coding");
        assert_eq!(category(&win("chrome.exe", "Lofi beats - YouTube - Google Chrome", "")), "video");
        assert_eq!(category(&win("chrome.exe", "Docs - Google Chrome", "")), "browsing");
        assert_eq!(
            category(&win("game.exe", "Game", r"D:\Steam\steamapps\common\Game\game.exe")),
            "gaming"
        );
        assert_eq!(category(&win("notepad.exe", "notes", "")), "other");
    }

    #[test]
    fn private_windows_hide_everything() {
        let (app, _, cat, title) = classify(
            &win("msedge.exe", "New InPrivate tab - Microsoft Edge", ""),
            &Settings::default(),
        );
        assert_eq!((app.as_str(), cat.as_str(), title), ("private", "private", None));
    }

    #[test]
    fn empty_rule_matches_nothing() {
        assert!(!ActivityRule::default().matches(&win("code.exe", "x", "")));
    }

    #[test]
    fn watching_video_never_counts_as_away() {
        assert_eq!(presence_for(AWAY_AFTER_SECS + 1, Some("video")), "active");
        assert_eq!(presence_for(AWAY_AFTER_SECS + 1, Some("coding")), "away");
        assert_eq!(presence_for(IDLE_AFTER_SECS, Some("coding")), "idle");
        assert_eq!(presence_for(10, Some("coding")), "active");
    }

    #[test]
    fn prompt_context_respects_title_sharing_and_privacy() {
        let mut s = Settings::default();
        let a = Activity {
            enabled: true,
            app: Some("code.exe".into()),
            label: Some("VS Code".into()),
            category: Some("coding".into()),
            title: Some("secret.rs".into()),
            presence: "active".into(),
            since: now_millis() - 40 * 60_000,
        };
        let line = prompt_context(&a, &s).unwrap();
        assert!(line.contains("VS Code (coding)"), "{line}");
        assert!(line.contains("about 40 minutes"), "{line}");
        assert!(!line.contains("secret.rs"));

        s.share_titles = true;
        assert!(prompt_context(&a, &s).unwrap().contains("secret.rs"));

        let private = Activity {
            category: Some("private".into()),
            ..a
        };
        assert_eq!(prompt_context(&private, &s), None);
    }

    #[test]
    fn friendly_names() {
        assert_eq!(friendly_name("code.exe"), "VS Code");
        assert_eq!(friendly_name("notepad.exe"), "Notepad");
    }
}
