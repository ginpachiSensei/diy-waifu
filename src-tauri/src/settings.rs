//! User-editable settings, stored as one JSON blob in the `settings` table.
//! New fields need a default so older saved settings still load.

use crate::activity::{default_privacy_rules, default_rules, ActivityRule};
use crate::db::Db;
use serde::{Deserialize, Serialize};

const KEY: &str = "settings";

pub const DEFAULT_MODEL: &str = "qwen2.5:3b";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReplyLength {
    Short,
    #[default]
    Medium,
    Long,
}

impl ReplyLength {
    pub fn instruction(self) -> &'static str {
        match self {
            ReplyLength::Short => "1-2 short sentences",
            ReplyLength::Medium => "2-4 sentences",
            ReplyLength::Long => "one short paragraph",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub model: String,
    pub companion_name: String,
    pub user_name: String,
    pub personality: String,
    pub reply_length: ReplyLength,
    /// Pause automatically while one of `heavy_apps` is running
    pub auto_pause: bool,
    /// Process names, matched case-insensitively (e.g. "blender.exe")
    pub heavy_apps: Vec<String>,
    pub pet_visible: bool,
    /// Watch which app is in front (see activity.rs)
    pub screen_awareness: bool,
    /// Include the window title in what the model is told
    pub share_titles: bool,
    pub activity_rules: Vec<ActivityRule>,
    pub privacy_rules: Vec<ActivityRule>,
    /// Let the model search the web, read pages and check the weather
    pub internet_access: bool,
    /// Brave Search API key (empty: keyless DuckDuckGo Instant Answers)
    pub brave_api_key: String,
    /// Ask yes/no before each internet lookup (see ask.rs)
    pub ask_before_lookup: bool,
    /// Learn facts about the user from chat in the background
    pub memory_learning: bool,
    /// Speak first: greetings, welcome back, break nudges, check-ins
    pub proactive: bool,
    /// Minimum minutes between unprompted lines
    pub proactive_gap_minutes: u32,
    /// Suggest a break after this long coding or creating
    pub break_after_minutes: u32,
    /// "HH:MM"; no unprompted lines (except reminders) between these
    pub quiet_start: String,
    pub quiet_end: String,
    /// Push-to-talk speech input (voice.rs)
    pub voice_input: bool,
    /// Whisper model id from voice::MODELS
    pub whisper_model: String,
    /// Hold this to talk, e.g. "Ctrl+Shift+Space"
    pub ptt_shortcut: String,
    /// Listen all the time for the wake phrase (see handsfree.rs)
    pub hands_free: bool,
    /// Empty: "hey <companion name>"
    pub wake_word: String,
    /// Read replies aloud from the pet (with tts_engine)
    pub speak_replies: bool,
    /// Windows voice name; empty = the default voice
    pub tts_voice: String,
    pub tts_rate: f32,
    pub tts_pitch: f32,
    /// "windows" (speechSynthesis in the webview) or "piper" (see tts.rs)
    pub tts_engine: String,
    pub piper_voice: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            companion_name: "Mai".to_string(),
            user_name: String::new(),
            personality: "You are cheerful, curious and a little playful. You care about the \
                          user and like hearing about their day."
                .to_string(),
            reply_length: ReplyLength::Medium,
            auto_pause: true,
            heavy_apps: ["blender.exe", "unity.exe", "unityhub.exe"]
                .map(String::from)
                .to_vec(),
            pet_visible: true,
            screen_awareness: true,
            share_titles: false,
            activity_rules: default_rules(),
            privacy_rules: default_privacy_rules(),
            internet_access: true,
            brave_api_key: String::new(),
            ask_before_lookup: false,
            memory_learning: true,
            proactive: true,
            proactive_gap_minutes: 45,
            break_after_minutes: 120,
            quiet_start: "23:00".into(),
            quiet_end: "08:00".into(),
            voice_input: true,
            whisper_model: "base.en".into(),
            ptt_shortcut: "Ctrl+Shift+Space".into(),
            speak_replies: false,
            tts_voice: String::new(),
            tts_rate: 1.0,
            tts_pitch: 1.1,
            tts_engine: "windows".into(),
            hands_free: false,
            wake_word: String::new(),
            piper_voice: "en_US-amy-medium".into(),
        }
    }
}

impl Settings {
    pub fn load(db: &Db) -> Self {
        match db.get_setting(KEY) {
            Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_else(|e| {
                log::warn!("Saved settings unreadable ({e}), using defaults");
                Self::default()
            }),
            Ok(None) => Self::default(),
            Err(e) => {
                log::error!("Couldn't load settings: {e}");
                Self::default()
            }
        }
    }

    pub fn save(&self, db: &Db) -> Result<(), String> {
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        db.set_setting(KEY, &json)
    }

    /// Trims fields and fills blanks that would break the prompt.
    pub fn cleaned(mut self) -> Result<Self, String> {
        self.model = self.model.trim().to_string();
        self.companion_name = self.companion_name.trim().to_string();
        self.user_name = self.user_name.trim().to_string();
        self.personality = self.personality.trim().to_string();
        self.brave_api_key = self.brave_api_key.trim().to_string();
        self.proactive_gap_minutes = self.proactive_gap_minutes.clamp(5, 24 * 60);
        self.ptt_shortcut = self.ptt_shortcut.trim().to_string();
        self.tts_rate = self.tts_rate.clamp(0.5, 2.0);
        self.tts_pitch = self.tts_pitch.clamp(0.5, 2.0);
        self.wake_word = self.wake_word.trim().chars().take(40).collect();
        if self.tts_engine != "piper" {
            self.tts_engine = "windows".into();
        }
        if !crate::voice::MODELS.iter().any(|m| m.0 == self.whisper_model) {
            self.whisper_model = Self::default().whisper_model;
        }
        self.break_after_minutes = self.break_after_minutes.clamp(15, 12 * 60);
        for t in [&self.quiet_start, &self.quiet_end] {
            if chrono::NaiveTime::parse_from_str(t.trim(), "%H:%M").is_err() {
                return Err(format!("Quiet hours need times like 23:00, not \"{t}\""));
            }
        }
        if self.model.is_empty() {
            return Err("Pick a model first".into());
        }
        self.heavy_apps = self
            .heavy_apps
            .iter()
            .map(|a| a.trim().to_lowercase())
            .filter(|a| !a.is_empty())
            .collect();
        let clean_rules = |rules: Vec<ActivityRule>| -> Vec<ActivityRule> {
            rules
                .into_iter()
                .map(|r| ActivityRule {
                    category: r.category.trim().to_lowercase(),
                    app: r.app.trim().to_lowercase(),
                    title: r.title.trim().to_string(),
                    path: r.path.trim().to_string(),
                })
                .filter(|r| !(r.app.is_empty() && r.title.is_empty() && r.path.is_empty()))
                .collect()
        };
        self.activity_rules = clean_rules(self.activity_rules)
            .into_iter()
            .filter(|r| !r.category.is_empty())
            .collect();
        self.privacy_rules = clean_rules(self.privacy_rules)
            .into_iter()
            .map(|r| ActivityRule {
                category: "private".into(),
                ..r
            })
            .collect();
        self.heavy_apps.sort();
        self.heavy_apps.dedup();
        if self.companion_name.is_empty() {
            self.companion_name = Self::default().companion_name;
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_missing_fields_still_load() {
        let s: Settings = serde_json::from_str(r#"{"companionName":"Rin"}"#).unwrap();
        assert_eq!(s.companion_name, "Rin");
        assert_eq!(s.model, DEFAULT_MODEL);
        assert_eq!(s.reply_length, ReplyLength::Medium);
    }

    #[test]
    fn round_trips_through_db() {
        let db = Db::open_in_memory().unwrap();
        let s = Settings {
            user_name: "Satyam".into(),
            reply_length: ReplyLength::Short,
            ..Settings::default()
        };
        s.save(&db).unwrap();
        assert_eq!(Settings::load(&db), s);
    }
}
