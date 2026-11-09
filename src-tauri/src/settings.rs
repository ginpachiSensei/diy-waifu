//! User-editable settings, stored as one JSON blob in the `settings` table.
//! New fields need a default so older saved settings still load.

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
        if self.model.is_empty() {
            return Err("Pick a model first".into());
        }
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
