//! SQLite storage for chat history and settings. The file lives in the app's
//! data folder (`companion.db`). Schema changes go at the end of `MIGRATIONS`;
//! never edit one that has shipped.

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
"];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StoredMessage {
    pub id: i64,
    pub ts: i64,
    pub role: String,
    pub text: String,
    pub emotion: Option<String>,
    pub source: String,
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
    ) -> Result<StoredMessage, String> {
        let ts = now_millis();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages (ts, role, text, emotion, source) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ts, role, text, emotion, source],
        )
        .map_err(err)?;
        Ok(StoredMessage {
            id: conn.last_insert_rowid(),
            ts,
            role: role.to_string(),
            text: text.to_string(),
            emotion: emotion.map(str::to_string),
            source: source.to_string(),
        })
    }

    /// The newest `limit` messages, oldest first.
    pub fn recent_messages(&self, limit: u32) -> Result<Vec<StoredMessage>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, role, text, emotion, source FROM messages
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
            db.add_message("user", &format!("m{i}"), None, "panel").unwrap();
        }
        let recent = db.recent_messages(3).unwrap();
        let texts: Vec<_> = recent.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["m2", "m3", "m4"]);

        db.clear_messages().unwrap();
        assert!(db.recent_messages(10).unwrap().is_empty());
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
