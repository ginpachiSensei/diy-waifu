//! SQLite storage for chat history and settings. The file lives in the app's
//! data folder (`companion.db`). Schema changes go at the end of `MIGRATIONS`;
//! never edit one that has shipped.

use crate::tools::Source;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;

/// Each entry upgrades the schema by one version (tracked in `PRAGMA user_version`).
const MIGRATIONS: &[&str] = &["
    CREATE TABLE messages (
        id      INTEGER PRIMARY KEY AUTOINCREMENT,
        ts      INTEGER NOT NULL,          -- unix millis
        role    TEXT NOT NULL,             -- 'user' | 'assistant'
        text    TEXT NOT NULL,
        emotion TEXT,
        source  TEXT NOT NULL              -- 'panel' | 'phone' | 'esp32' | 'pet' | 'companion' | ...
    );
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
", "
    -- One row per stretch of time in one app + category. No window titles.
    CREATE TABLE activity_sessions (
        id       INTEGER PRIMARY KEY AUTOINCREMENT,
        app      TEXT NOT NULL,
        category TEXT NOT NULL,
        start_ts INTEGER NOT NULL,
        end_ts   INTEGER NOT NULL
    );
    CREATE INDEX activity_sessions_start ON activity_sessions(start_ts);
", "
    -- JSON array of {title, url} for replies that used the web
    ALTER TABLE messages ADD COLUMN sources TEXT;
"];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StoredMessage {
    pub id: i64,
    pub ts: i64,
    pub role: String,
    pub text: String,
    pub emotion: Option<String>,
    pub source: String,
    /// Web pages the reply was based on (empty for most messages)
    pub sources: Vec<Source>,
}

pub struct Db {
    conn: Mutex<Connection>,
}

fn err(e: impl std::fmt::Display) -> String {
    format!("database error: {e}")
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::init(Connection::open(path).map_err(err)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        Self::init(Connection::open_in_memory().map_err(err)?)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        // journal_mode returns a row, so it can't go through execute()
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(err)?;
        migrate(&conn).map_err(err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while holding the lock doesn't corrupt SQLite, so keep going
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn add_message(
        &self,
        role: &str,
        text: &str,
        emotion: Option<&str>,
        source: &str,
        sources: &[Source],
    ) -> Result<StoredMessage, String> {
        let ts = now_millis();
        let sources_json = (!sources.is_empty())
            .then(|| serde_json::to_string(sources).ok())
            .flatten();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages (ts, role, text, emotion, source, sources) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ts, role, text, emotion, source, sources_json],
        )
        .map_err(err)?;
        Ok(StoredMessage {
            id: conn.last_insert_rowid(),
            ts,
            role: role.to_string(),
            text: text.to_string(),
            emotion: emotion.map(str::to_string),
            source: source.to_string(),
            sources: sources.to_vec(),
        })
    }

    /// The newest `limit` messages, oldest first.
    pub fn recent_messages(&self, limit: u32) -> Result<Vec<StoredMessage>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, role, text, emotion, source, sources FROM messages
                 ORDER BY id DESC LIMIT ?1",
            )
            .map_err(err)?;
        let mut rows: Vec<StoredMessage> = stmt
            .query_map([limit], |r| {
                Ok(StoredMessage {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    role: r.get(2)?,
                    text: r.get(3)?,
                    emotion: r.get(4)?,
                    source: r.get(5)?,
                    sources: r
                        .get::<_, Option<String>>(6)?
                        .and_then(|j| serde_json::from_str(&j).ok())
                        .unwrap_or_default(),
                })
            })
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        rows.reverse();
        Ok(rows)
    }

    pub fn clear_messages(&self) -> Result<(), String> {
        self.conn()
            .execute("DELETE FROM messages", [])
            .map(|_| ())
            .map_err(err)
    }

    // ---- Activity sessions ----

    pub fn start_session(&self, app: &str, category: &str, ts: i64) -> Result<i64, String> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO activity_sessions (app, category, start_ts, end_ts) VALUES (?1, ?2, ?3, ?3)",
            params![app, category, ts],
        )
        .map_err(err)?;
        Ok(conn.last_insert_rowid())
    }

    pub fn touch_session(&self, id: i64, end_ts: i64) -> Result<(), String> {
        self.conn()
            .execute(
                "UPDATE activity_sessions SET end_ts = ?2 WHERE id = ?1",
                params![id, end_ts],
            )
            .map(|_| ())
            .map_err(err)
    }

    pub fn prune_sessions(&self, before_ts: i64) -> Result<(), String> {
        self.conn()
            .execute("DELETE FROM activity_sessions WHERE end_ts < ?1", [before_ts])
            .map(|_| ())
            .map_err(err)
    }

    /// Total milliseconds per category in sessions overlapping [from, now), biggest first.
    pub fn category_totals_since(&self, from: i64) -> Result<Vec<(String, i64)>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT category, SUM(end_ts - MAX(start_ts, ?1)) AS ms FROM activity_sessions
                 WHERE end_ts > ?1 GROUP BY category ORDER BY ms DESC",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([from], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        Ok(rows)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, String> {
        self.conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(err)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn()
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
            .map_err(err)
    }
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_returns_recent_messages_in_order() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..5 {
            db.add_message("user", &format!("m{i}"), None, "panel", &[]).unwrap();
        }
        let recent = db.recent_messages(3).unwrap();
        let texts: Vec<_> = recent.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["m2", "m3", "m4"]);

        let src = vec![Source {
            title: "Rust".into(),
            url: "https://rust-lang.org/".into(),
        }];
        db.add_message("assistant", "with sources", Some("happy"), "companion", &src).unwrap();
        assert_eq!(db.recent_messages(1).unwrap()[0].sources, src);

        db.clear_messages().unwrap();
        assert!(db.recent_messages(10).unwrap().is_empty());
    }

    #[test]
    fn activity_totals_clip_to_window() {
        let db = Db::open_in_memory().unwrap();
        let a = db.start_session("code.exe", "coding", 1_000).unwrap();
        db.touch_session(a, 5_000).unwrap();
        let b = db.start_session("chrome.exe", "video", 5_000).unwrap();
        db.touch_session(b, 6_000).unwrap();
        // Only the part after t=3000 counts
        assert_eq!(
            db.category_totals_since(3_000).unwrap(),
            vec![("coding".to_string(), 2_000), ("video".to_string(), 1_000)]
        );
        db.prune_sessions(5_500).unwrap();
        assert_eq!(db.category_totals_since(0).unwrap().len(), 1);
    }

    #[test]
    fn settings_upsert() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_setting("k").unwrap(), None);
        db.set_setting("k", "a").unwrap();
        db.set_setting("k", "b").unwrap();
        assert_eq!(db.get_setting("k").unwrap().as_deref(), Some("b"));
    }
}
