//! Speaking first. Every 20s this checks:
//! - **reminders** that are due: always delivered, even in quiet hours or while
//!   paused (as plain text then, since the model is unloaded);
//! - and, with `settings.proactive` on, the moments worth a short line:
//!   a greeting after startup, welcome back after being away, a break nudge
//!   after a long stretch of coding or creative work, and a check-in after a
//!   long silence.
//!
//! Non-reminder lines respect quiet hours and a minimum gap, and never
//! interrupt gaming, video, a conversation in progress, or a paused companion.

use crate::activity::Activity;
use crate::db::{now_millis, Db};
use crate::settings::Settings;
use crate::AppState;
use chrono::{Local, NaiveTime, Timelike};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{Emitter, Manager};

const TICK: Duration = Duration::from_secs(20);
/// Wait this long after launch before greeting (Ollama and the model need to load)
const GREET_AFTER_MS: i64 = 30_000;
/// Don't butt in if the user chatted this recently
const CONVERSATION_QUIET_MS: i64 = 10 * 60_000;
/// Count as "away" only if gone at least this long
const MIN_AWAY_MS: i64 = 20 * 60_000;
const SILENCE_MS: i64 = 3 * 3_600_000;
/// Categories where the companion stays quiet
const DO_NOT_DISTURB: &[&str] = &["gaming", "video", "private"];
/// Categories where a break nudge makes sense
const BREAK_CATEGORIES: &[&str] = &["coding", "creative"];

fn parse_clock(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// Whether `now` is inside quiet hours start..end (which may wrap midnight)
pub fn in_quiet_hours(now: NaiveTime, start: &str, end: &str) -> bool {
    let (Some(s), Some(e)) = (parse_clock(start), parse_clock(end)) else {
        return false;
    };
    if s == e {
        false
    } else if s < e {
        now >= s && now < e
    } else {
        now >= s || now < e
    }
}

fn part_of_day() -> &'static str {
    match Local::now().hour() {
        5..=11 => "morning",
        12..=16 => "afternoon",
        17..=21 => "evening",
        _ => "night",
    }
}

struct State {
    started: i64,
    greeted: bool,
    last_spoke: i64,
    /// When the current away stretch began
    away_since: Option<i64>,
    /// Set when the user returns from a long absence, until we've said hi
    welcome_back: Option<i64>,
    /// `Activity::since` of the stretch we already nudged about
    break_nudged_for: i64,
}

pub async fn run(app: tauri::AppHandle) {
    let mut st = State {
        started: now_millis(),
        greeted: false,
        last_spoke: 0,
        away_since: None,
        welcome_back: None,
        break_nudged_for: 0,
    };
    loop {
        tokio::time::sleep(TICK).await;
        deliver_reminders(&app).await;
        if let Some(instruction) = next_line(&app, &mut st) {
            match crate::companion::speak_first(&app, &instruction, None).await {
                Ok(_) => st.last_spoke = now_millis(),
                Err(e) if e == "busy" => {}
                Err(e) => {
                    log::warn!("Proactive line failed: {e}");
                    // Don't retry in a tight loop (e.g. the model isn't downloaded)
                    st.last_spoke = now_millis();
                }
            }
        }
    }
}

async fn deliver_reminders(app: &tauri::AppHandle) {
    let db = app.state::<Db>();
    let due = match db.due_reminders(now_millis()) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("Couldn't read reminders: {e}");
            return;
        }
    };
    for r in due {
        // Mark first, so a crash mid-delivery doesn't repeat it forever
        db.finish_reminder(r.id).ok();
        app.emit("memory:changed", ()).ok();
        let plain = format!("Hey, reminder: {}!", r.text);
        log::info!("Delivering reminder: {}", r.text);
        if app.state::<AppState>().paused.load(Ordering::Relaxed) {
            // The model is unloaded while paused; say it plainly
            if let Ok(m) = db.add_message("assistant", &plain, Some("neutral"), "proactive", &[]) {
                app.emit("chat:message", &m).ok();
                let msg = serde_json::json!({ "type": "reply", "emotion": "neutral", "text": plain });
                let _ = app.state::<AppState>().broadcast_tx.send(msg.to_string());
            }
            continue;
        }
        let instruction = format!(
            "It's time for a reminder they asked you to give: \"{}\". Remind them now, clearly.",
            r.text
        );
        crate::companion::speak_first(app, &instruction, Some(&plain)).await.ok();
    }
}

/// Decides whether to say something now, and what the moment is
fn next_line(app: &tauri::AppHandle, st: &mut State) -> Option<String> {
    let now = now_millis();
    let db = app.state::<Db>();
    let settings = Settings::load(&db);
    let activity: Activity = app.state::<AppState>().activity.lock().unwrap().clone();

    // Track away stretches even when we won't speak
    if activity.presence == "away" {
        st.away_since.get_or_insert(now);
    } else if let Some(since) = st.away_since.take() {
        if now - since >= MIN_AWAY_MS {
            st.welcome_back = Some(now - since);
        }
    }

    if !settings.proactive {
        return None;
    }
    let state = app.state::<AppState>();
    if state.paused.load(Ordering::Relaxed) || *state.ollama_status.lock().unwrap() != "ready" {
        return None;
    }
    if in_quiet_hours(Local::now().time(), &settings.quiet_start, &settings.quiet_end) {
        return None;
    }
    if activity.presence != "active"
        || activity.category.as_deref().is_some_and(|c| DO_NOT_DISTURB.contains(&c))
    {
        return None;
    }
    let last_chat = db.last_message_ts().ok().flatten().unwrap_or(0);
    if now - last_chat < CONVERSATION_QUIET_MS {
        // Talking already; a greeting now would be odd
        st.greeted = true;
        st.welcome_back = None;
        return None;
    }
    if now - st.last_spoke < settings.proactive_gap_minutes as i64 * 60_000 {
        return None;
    }

    let user = if settings.user_name.is_empty() { "the user".to_string() } else { settings.user_name.clone() };
    let doing = match (&activity.label, &activity.category) {
        (Some(l), Some(c)) if c != "other" => format!(" They're in {l} ({c}) right now."),
        (Some(l), _) => format!(" They're in {l} right now."),
        _ => String::new(),
    };

    if !st.greeted && now - st.started >= GREET_AFTER_MS {
        st.greeted = true;
        return Some(format!(
            "You just woke up on {user}'s desktop this {}. Greet them warmly and briefly.{doing}",
            part_of_day()
        ));
    }
    if let Some(away_ms) = st.welcome_back.take() {
        return Some(format!(
            "{user} just came back after being away for about {} minutes. Welcome them back.{doing}",
            away_ms / 60_000
        ));
    }
    if let (Some(cat), Some(label)) = (&activity.category, &activity.label) {
        let stretch = now - activity.since;
        if BREAK_CATEGORIES.contains(&cat.as_str())
            && stretch >= settings.break_after_minutes as i64 * 60_000
            && st.break_nudged_for != activity.since
        {
            st.break_nudged_for = activity.since;
            return Some(format!(
                "{user} has been {cat} in {label} for {} minutes straight. Gently suggest a short \
                 break: stretch, water, rest their eyes.",
                stretch / 60_000
            ));
        }
    }
    if now - last_chat >= SILENCE_MS && now - st.last_spoke >= SILENCE_MS {
        return Some(format!(
            "You haven't talked for {} hours. Check in with something short and friendly.{doing}",
            (now - last_chat) / 3_600_000
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn quiet_hours_wrap_midnight() {
        assert!(in_quiet_hours(t(23, 30), "23:00", "08:00"));
        assert!(in_quiet_hours(t(3, 0), "23:00", "08:00"));
        assert!(!in_quiet_hours(t(8, 0), "23:00", "08:00"));
        assert!(!in_quiet_hours(t(15, 0), "23:00", "08:00"));
        assert!(in_quiet_hours(t(13, 0), "12:00", "14:00"));
        assert!(!in_quiet_hours(t(13, 0), "09:00", "09:00"));
        assert!(!in_quiet_hours(t(13, 0), "bad", "08:00"));
    }
}
