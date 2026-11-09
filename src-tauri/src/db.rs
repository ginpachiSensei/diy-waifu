//! SQLite storage for chat history and settings. The file lives in the app's
//! data folder (`companion.db`). Schema changes go at the end of `MIGRATIONS`;
//! never edit one that has shipped.

use crate::tools::Source;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
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
", "
    -- Long-term memory (memory.rs)
    CREATE TABLE facts (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        text      TEXT NOT NULL,
        origin    TEXT NOT NULL,         -- 'auto' | 'user'
        created   INTEGER NOT NULL,
        last_used INTEGER NOT NULL,
        embedding BLOB                   -- f32 little-endian, NULL until embedded
    );
    CREATE TABLE summaries (
        id      INTEGER PRIMARY KEY AUTOINCREMENT,
        ts      INTEGER NOT NULL,
        from_id INTEGER NOT NULL,        -- message ids covered, inclusive
        to_id   INTEGER NOT NULL,
        text    TEXT NOT NULL
    );
    CREATE TABLE message_embeddings (
        message_id INTEGER PRIMARY KEY,
        embedding  BLOB NOT NULL
    );
    CREATE TABLE reminders (
        id      INTEGER PRIMARY KEY AUTOINCREMENT,
        due_ts  INTEGER NOT NULL,
        text    TEXT NOT NULL,
        created INTEGER NOT NULL,
        done    INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX reminders_due ON reminders(done, due_ts);
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
            .query_map([limit], row_to_message)
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        rows.reverse();
        Ok(rows)
    }

    pub fn clear_messages(&self) -> Result<(), String> {
        // Summaries and embeddings describe these messages, so they go too
        self.conn()
            .execute_batch(
                "DELETE FROM messages; DELETE FROM message_embeddings; DELETE FROM summaries;",
            )
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

/// Columns: id, ts, role, text, emotion, source, sources
fn row_to_message(r: &rusqlite::Row) -> rusqlite::Result<StoredMessage> {
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
}

// ---------------------------------------------------------------------------
// Long-term memory: facts, summaries, message embeddings, reminders
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    pub id: i64,
    pub text: String,
    /// "auto" (learned from chat) | "user" (said "remember that..." or typed in the Memory tab)
    pub origin: String,
    pub created: i64,
    pub last_used: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: i64,
    pub ts: i64,
    /// Message ids covered, inclusive
    pub from_id: i64,
    pub to_id: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    pub id: i64,
    pub due_ts: i64,
    pub text: String,
    pub created: i64,
    pub done: bool,
}

/// A fact and its embedding, if it has one
pub type FactWithEmbedding = (Fact, Option<Vec<f32>>);

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn row_to_fact(r: &rusqlite::Row) -> rusqlite::Result<Fact> {
    Ok(Fact {
        id: r.get(0)?,
        text: r.get(1)?,
        origin: r.get(2)?,
        created: r.get(3)?,
        last_used: r.get(4)?,
    })
}

impl Db {
    // ---- Facts ----

    pub fn add_fact(&self, text: &str, origin: &str, embedding: Option<&[f32]>) -> Result<Fact, String> {
        let ts = now_millis();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO facts (text, origin, created, last_used, embedding) VALUES (?1, ?2, ?3, ?3, ?4)",
            params![text, origin, ts, embedding.map(to_blob)],
        )
        .map_err(err)?;
        Ok(Fact {
            id: conn.last_insert_rowid(),
            text: text.to_string(),
            origin: origin.to_string(),
            created: ts,
            last_used: ts,
        })
    }

    /// Newest first
    pub fn list_facts(&self) -> Result<Vec<Fact>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, text, origin, created, last_used FROM facts ORDER BY id DESC")
            .map_err(err)?;
        let rows = stmt
            .query_map([], row_to_fact)
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err);
        rows
    }

    /// Every fact with its embedding, if it has one
    pub fn facts_with_embeddings(&self) -> Result<Vec<FactWithEmbedding>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, text, origin, created, last_used, embedding FROM facts ORDER BY id DESC")
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((row_to_fact(r)?, r.get::<_, Option<Vec<u8>>>(5)?.map(|b| from_blob(&b))))
            })
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err);
        rows
    }

    pub fn update_fact(&self, id: i64, text: &str, embedding: Option<&[f32]>) -> Result<(), String> {
        self.conn()
            .execute(
                "UPDATE facts SET text = ?2, embedding = ?3 WHERE id = ?1",
                params![id, text, embedding.map(to_blob)],
            )
            .map(|_| ())
            .map_err(err)
    }

    pub fn set_fact_embedding(&self, id: i64, embedding: &[f32]) -> Result<(), String> {
        self.conn()
            .execute(
                "UPDATE facts SET embedding = ?2 WHERE id = ?1",
                params![id, to_blob(embedding)],
            )
            .map(|_| ())
            .map_err(err)
    }

    pub fn delete_fact(&self, id: i64) -> Result<(), String> {
        self.conn()
            .execute("DELETE FROM facts WHERE id = ?1", [id])
            .map(|_| ())
            .map_err(err)
    }

    pub fn touch_facts(&self, ids: &[i64]) -> Result<(), String> {
        let ts = now_millis();
        let conn = self.conn();
        for id in ids {
            conn.execute("UPDATE facts SET last_used = ?2 WHERE id = ?1", params![id, ts])
                .map_err(err)?;
        }
        Ok(())
    }

    // ---- Summaries ----

    pub fn add_summary(&self, from_id: i64, to_id: i64, text: &str) -> Result<Summary, String> {
        let ts = now_millis();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO summaries (ts, from_id, to_id, text) VALUES (?1, ?2, ?3, ?4)",
            params![ts, from_id, to_id, text],
        )
        .map_err(err)?;
        Ok(Summary {
            id: conn.last_insert_rowid(),
            ts,
            from_id,
            to_id,
            text: text.to_string(),
        })
    }

    /// Newest first
    pub fn list_summaries(&self, limit: u32) -> Result<Vec<Summary>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, ts, from_id, to_id, text FROM summaries ORDER BY to_id DESC LIMIT ?1")
            .map_err(err)?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(Summary {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    from_id: r.get(2)?,
                    to_id: r.get(3)?,
                    text: r.get(4)?,
                })
            })
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err);
        rows
    }

    pub fn delete_summary(&self, id: i64) -> Result<(), String> {
        self.conn()
            .execute("DELETE FROM summaries WHERE id = ?1", [id])
            .map(|_| ())
            .map_err(err)
    }

    /// Highest message id already covered by a summary (0 if none)
    pub fn last_summarized_id(&self) -> Result<i64, String> {
        self.conn()
            .query_row("SELECT COALESCE(MAX(to_id), 0) FROM summaries", [], |r| r.get(0))
            .map_err(err)
    }

    // ---- Messages, for memory ----

    /// Id of the `n`th newest message (1 = newest)
    pub fn nth_newest_message_id(&self, n: u32) -> Result<Option<i64>, String> {
        self.conn()
            .query_row(
                "SELECT id FROM messages ORDER BY id DESC LIMIT 1 OFFSET ?1",
                [n.saturating_sub(1)],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)
    }

    /// Messages with after_id < id < before_id, oldest first
    pub fn messages_between(&self, after_id: i64, before_id: i64, limit: u32) -> Result<Vec<StoredMessage>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, role, text, emotion, source, sources FROM messages
                 WHERE id > ?1 AND id < ?2 ORDER BY id LIMIT ?3",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map(params![after_id, before_id, limit], row_to_message)
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err);
        rows
    }

    pub fn messages_by_ids(&self, ids: &[i64]) -> Result<Vec<StoredMessage>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, ts, role, text, emotion, source, sources FROM messages WHERE id = ?1")
            .map_err(err)?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(m) = stmt.query_row([id], row_to_message).optional().map_err(err)? {
                out.push(m);
            }
        }
        Ok(out)
    }

    pub fn last_message_ts(&self) -> Result<Option<i64>, String> {
        self.conn()
            .query_row("SELECT MAX(ts) FROM messages", [], |r| r.get(0))
            .map_err(err)
    }

    pub fn set_message_embedding(&self, id: i64, embedding: &[f32]) -> Result<(), String> {
        self.conn()
            .execute(
                "INSERT OR REPLACE INTO message_embeddings (message_id, embedding) VALUES (?1, ?2)",
                params![id, to_blob(embedding)],
            )
            .map(|_| ())
            .map_err(err)
    }

    /// Messages that don't have an embedding yet, newest first
    pub fn unembedded_messages(&self, limit: u32) -> Result<Vec<(i64, String)>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, text FROM messages WHERE id NOT IN (SELECT message_id FROM message_embeddings)
                 ORDER BY id DESC LIMIT ?1",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([limit], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err);
        rows
    }

    /// (message id, embedding) for messages older than `before_id`
    pub fn message_embeddings_before(&self, before_id: i64) -> Result<Vec<(i64, Vec<f32>)>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT message_id, embedding FROM message_embeddings WHERE message_id < ?1")
            .map_err(err)?;
        let rows = stmt
            .query_map([before_id], |r| Ok((r.get(0)?, from_blob(&r.get::<_, Vec<u8>>(1)?))))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err);
        rows
    }

    pub fn count(&self, table: &'static str) -> Result<i64, String> {
        // `table` is always a literal from our own code, never user input
        self.conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .map_err(err)
    }

    /// Forgets facts, summaries and message embeddings (not the chat history)
    pub fn clear_memory(&self) -> Result<(), String> {
        self.conn()
            .execute_batch("DELETE FROM facts; DELETE FROM summaries; DELETE FROM message_embeddings;")
            .map_err(err)
    }

    // ---- Reminders ----

    pub fn add_reminder(&self, due_ts: i64, text: &str) -> Result<Reminder, String> {
        let created = now_millis();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO reminders (due_ts, text, created, done) VALUES (?1, ?2, ?3, 0)",
            params![due_ts, text, created],
        )
        .map_err(err)?;
        Ok(Reminder {
            id: conn.last_insert_rowid(),
            due_ts,
            text: text.to_string(),
            created,
            done: false,
        })
    }

    fn reminders_where(&self, clause: &str, param: i64) -> Result<Vec<Reminder>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id, due_ts, text, created, done FROM reminders WHERE {clause} ORDER BY due_ts"
            ))
            .map_err(err)?;
        let rows = stmt
            .query_map([param], |r| {
                Ok(Reminder {
                    id: r.get(0)?,
                    due_ts: r.get(1)?,
                    text: r.get(2)?,
                    created: r.get(3)?,
                    done: r.get::<_, i64>(4)? != 0,
                })
            })
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err);
        rows
    }

    pub fn due_reminders(&self, now: i64) -> Result<Vec<Reminder>, String> {
        self.reminders_where("done = 0 AND due_ts <= ?1", now)
    }

    pub fn pending_reminders(&self) -> Result<Vec<Reminder>, String> {
        self.reminders_where("done = 0 AND ?1 = ?1", 0)
    }

    pub fn finish_reminder(&self, id: i64) -> Result<(), String> {
        self.conn()
            .execute("UPDATE reminders SET done = 1 WHERE id = ?1", [id])
            .map(|_| ())
            .map_err(err)
    }

    pub fn delete_reminder(&self, id: i64) -> Result<(), String> {
        self.conn()
            .execute("DELETE FROM reminders WHERE id = ?1", [id])
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
    fn memory_tables_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let f = db.add_fact("The user likes momos", "auto", Some(&[0.5, -1.0])).unwrap();
        let all = db.facts_with_embeddings().unwrap();
        assert_eq!(all[0].0.text, "The user likes momos");
        assert_eq!(all[0].1.as_deref(), Some(&[0.5f32, -1.0][..]));
        db.update_fact(f.id, "The user loves momos", None).unwrap();
        assert_eq!(db.list_facts().unwrap()[0].text, "The user loves momos");

        for i in 0..5 {
            db.add_message("user", &format!("m{i}"), None, "panel", &[]).unwrap();
        }
        assert_eq!(db.nth_newest_message_id(2).unwrap(), Some(4));
        let mid = db.messages_between(1, 4, 10).unwrap();
        assert_eq!(mid.iter().map(|m| m.id).collect::<Vec<_>>(), vec![2, 3]);
        db.set_message_embedding(2, &[1.0]).unwrap();
        assert_eq!(db.unembedded_messages(10).unwrap().len(), 4);
        assert_eq!(db.message_embeddings_before(3).unwrap().len(), 1);

        db.add_summary(1, 3, "They talked about food.").unwrap();
        assert_eq!(db.last_summarized_id().unwrap(), 3);

        let r = db.add_reminder(1_000, "drink water").unwrap();
        db.add_reminder(i64::MAX, "later").unwrap();
        assert_eq!(db.due_reminders(2_000).unwrap(), vec![r.clone()]);
        db.finish_reminder(r.id).unwrap();
        assert!(db.due_reminders(2_000).unwrap().is_empty());
        assert_eq!(db.pending_reminders().unwrap().len(), 1);

        db.clear_memory().unwrap();
        assert!(db.list_facts().unwrap().is_empty());
        assert_eq!(db.count("message_embeddings").unwrap(), 0);
        assert_eq!(db.count("messages").unwrap(), 5);
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
