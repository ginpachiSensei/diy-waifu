//! Long-term memory.
//!
//! - **Facts** about the user ("The user's sister is called Priya"). Learned in
//!   the background from chat (`learn`), or added by saying "remember that ..."
//!   or in the Memory tab. Removed by "forget ..." or in the Memory tab.
//! - **Summaries** of chat that has scrolled out of the prompt window.
//! - **Recall**: each reply gets the relevant facts, the latest summaries, and
//!   a few older messages that match the new one by meaning.
//!
//! Matching by meaning uses embeddings from a small Ollama model
//! (`EMBED_MODEL`). Without it, memory still works: every fact (or the newest
//! ones) goes in, and "forget" matches by text.

use crate::db::{Db, Fact, StoredMessage, Summary};
use crate::ollama::{self, ChatMessage};
use crate::settings::Settings;
use regex::Regex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

pub const EMBED_MODEL: &str = "nomic-embed-text";

/// Up to this many facts, all of them go in every prompt
const FACTS_ALWAYS: usize = 12;
/// Beyond that, the most relevant ones (topped up with the newest)
const FACTS_TOP: usize = 8;
const FACT_MIN_SIM: f32 = 0.45;
/// Older messages recalled by meaning
const RECALL_TOP: usize = 3;
const RECALL_MIN_SIM: f32 = 0.62;
/// A new fact this close to an old one is a duplicate
const DUPLICATE_SIM: f32 = 0.9;
/// Learn facts after this many user messages
const LEARN_EVERY: u32 = 4;
/// Summarize once this many messages have scrolled out of the prompt...
const SUMMARIZE_MIN: usize = 20;
/// ...taking at most this many at a time
const SUMMARIZE_CHUNK: u32 = 40;

// ---------------------------------------------------------------------------
// Embeddings
// ---------------------------------------------------------------------------

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Whether the embedding model is downloaded. Checked at most every 2 minutes.
pub async fn embed_ready() -> bool {
    static CACHE: OnceLock<Mutex<Option<(Instant, bool)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Some((at, ready)) = *cache.lock().unwrap() {
        if at.elapsed() < Duration::from_secs(120) {
            return ready;
        }
    }
    let ready = ollama::list_models()
        .await
        .map(|ms| ms.iter().any(|m| m.name.split(':').next() == Some(EMBED_MODEL)))
        .unwrap_or(false);
    *cache.lock().unwrap() = Some((Instant::now(), ready));
    ready
}

/// nomic-embed-text expects these prefixes for best results
fn as_document(t: &str) -> String {
    format!("search_document: {t}")
}
fn as_query(t: &str) -> String {
    format!("search_query: {t}")
}

async fn embed_many(texts: Vec<String>) -> Option<Vec<Vec<f32>>> {
    if texts.is_empty() || !embed_ready().await {
        return None;
    }
    match ollama::embed(EMBED_MODEL, &texts).await {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("Embedding failed: {e}");
            None
        }
    }
}

pub async fn embed_document(text: &str) -> Option<Vec<f32>> {
    embed_many(vec![as_document(text)]).await?.pop()
}

async fn embed_query(text: &str) -> Option<Vec<f32>> {
    embed_many(vec![as_query(text)]).await?.pop()
}

// ---------------------------------------------------------------------------
// Recall
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct Recall {
    pub facts: Vec<Fact>,
    pub summaries: Vec<Summary>,
    pub messages: Vec<StoredMessage>,
}

/// Picks facts: all of them if there are few, otherwise the best matches for
/// `query` topped up with the newest. `facts` is newest first.
fn pick_facts(facts: Vec<(Fact, Option<Vec<f32>>)>, query: Option<&[f32]>) -> Vec<Fact> {
    if facts.len() <= FACTS_ALWAYS {
        return facts.into_iter().map(|(f, _)| f).collect();
    }
    let mut picked: Vec<Fact> = Vec::new();
    if let Some(q) = query {
        let mut scored: Vec<(f32, &Fact)> = facts
            .iter()
            .filter_map(|(f, e)| e.as_ref().map(|e| (cosine(q, e), f)))
            .filter(|(s, _)| *s >= FACT_MIN_SIM)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        picked.extend(scored.into_iter().take(FACTS_TOP).map(|(_, f)| f.clone()));
    }
    for (f, _) in &facts {
        if picked.len() >= FACTS_TOP {
            break;
        }
        if !picked.iter().any(|p| p.id == f.id) {
            picked.push(f.clone());
        }
    }
    picked
}

/// Memory for a reply to `query`. `window_start_id` is the oldest message
/// already in the prompt; recalled messages and summaries come from before it.
pub async fn recall(db: &Db, query: &str, window_start_id: i64) -> Recall {
    let q = embed_query(query).await;
    let facts = pick_facts(db.facts_with_embeddings().unwrap_or_default(), q.as_deref());
    db.touch_facts(&facts.iter().map(|f| f.id).collect::<Vec<_>>()).ok();

    let mut summaries: Vec<Summary> = db
        .list_summaries(2)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.to_id < window_start_id)
        .collect();
    summaries.reverse();

    let mut messages = Vec::new();
    if let Some(q) = &q {
        let mut scored: Vec<(f32, i64)> = db
            .message_embeddings_before(window_start_id)
            .unwrap_or_default()
            .iter()
            .map(|(id, e)| (cosine(q, e), *id))
            .filter(|(s, _)| *s >= RECALL_MIN_SIM)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut ids: Vec<i64> = scored.into_iter().take(RECALL_TOP).map(|(_, id)| id).collect();
        ids.sort();
        messages = db.messages_by_ids(&ids).unwrap_or_default();
    }
    Recall {
        facts,
        summaries,
        messages,
    }
}

fn day(ts: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts)
        .map(|d| d.with_timezone(&chrono::Local).format("%-d %b").to_string())
        .unwrap_or_default()
}

/// System-prompt lines for what was recalled
pub fn context_lines(r: &Recall, s: &Settings) -> Vec<String> {
    let user = if s.user_name.is_empty() { "the user" } else { s.user_name.as_str() };
    let mut out = Vec::new();
    if !r.facts.is_empty() {
        let list = r.facts.iter().map(|f| format!("- {}", f.text)).collect::<Vec<_>>().join("\n");
        out.push(format!(
            "Things you remember about {user} (bring them up naturally when relevant, don't list them):\n{list}"
        ));
    }
    if !r.summaries.is_empty() {
        let text = r.summaries.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
        out.push(format!("Summary of your earlier conversations: {text}"));
    }
    if !r.messages.is_empty() {
        let list = r
            .messages
            .iter()
            .map(|m| {
                let who = if m.role == "assistant" { "You" } else { "They" };
                format!("- [{}] {who}: {}", day(m.ts), m.text)
            })
            .collect::<Vec<_>>()
            .join("\n");
        out.push(format!("Older messages that may be relevant:\n{list}"));
    }
    out
}

// ---------------------------------------------------------------------------
// "remember that ..." / "forget ..."
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum Command {
    Remember(String),
    Forget(String),
}

pub fn parse_command(text: &str) -> Option<Command> {
    static RE: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (remember, forget) = RE.get_or_init(|| {
        (
            Regex::new(r"(?is)^\s*(?:please\s+|hey,?\s+)?remember(?:\s+that|\s*:)\s+(.+?)[\s.!]*$").unwrap(),
            Regex::new(r"(?is)^\s*(?:please\s+)?forget(?:\s+that|\s+about|\s*:)?\s+(.+?)[\s.!]*$").unwrap(),
        )
    });
    if let Some(c) = remember.captures(text) {
        return Some(Command::Remember(c[1].trim().to_string()));
    }
    if let Some(c) = forget.captures(text) {
        let what = c[1].trim();
        // "forget it" / "forget everything" aren't about one memory
        if !matches!(what.to_lowercase().as_str(), "it" | "that" | "everything" | "about it") {
            return Some(Command::Forget(what.to_string()));
        }
    }
    None
}

/// Carries out a command and returns a line telling the model what happened.
pub async fn apply_command(app: &tauri::AppHandle, db: &Db, cmd: &Command) -> String {
    let line = match cmd {
        Command::Remember(what) => {
            let text = format!("The user told you: {what}");
            let emb = embed_document(&text).await;
            match db.add_fact(&text, "user", emb.as_deref()) {
                Ok(_) => format!("You just saved this to your long-term memory: \"{what}\". Confirm it briefly."),
                Err(e) => format!("You tried to remember \"{what}\" but saving failed ({e}). Say so."),
            }
        }
        Command::Forget(what) => match find_fact(db, what).await {
            Some(f) => {
                db.delete_fact(f.id).ok();
                format!("You just deleted this memory: \"{}\". Confirm it briefly.", f.text)
            }
            None => format!("The user asked you to forget \"{what}\", but no memory matched. Say so."),
        },
    };
    app.emit("memory:changed", ()).ok();
    line
}

/// The stored fact that best matches `what`: by meaning if possible, else by text.
async fn find_fact(db: &Db, what: &str) -> Option<Fact> {
    let facts = db.facts_with_embeddings().ok()?;
    if let Some(q) = embed_query(what).await {
        let best = facts
            .iter()
            .filter_map(|(f, e)| e.as_ref().map(|e| (cosine(&q, e), f)))
            .max_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((s, f)) = best {
            if s >= 0.6 {
                return Some(f.clone());
            }
        }
    }
    let w = what.to_lowercase();
    facts
        .into_iter()
        .map(|(f, _)| f)
        .find(|f| f.text.to_lowercase().contains(&w))
}

// ---------------------------------------------------------------------------
// Background learning, after each exchange
// ---------------------------------------------------------------------------

static USER_MESSAGES_SINCE_LEARN: AtomicU32 = AtomicU32::new(0);

/// Called after every reply. Embeds new messages, learns facts every few
/// messages, and summarizes old chat. Skips if a previous run is still going.
pub fn after_exchange(app: &tauri::AppHandle) {
    static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    USER_MESSAGES_SINCE_LEARN.fetch_add(1, Ordering::Relaxed);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Ok(_guard) = RUNNING.try_lock() else {
            return;
        };
        let db = app.state::<Db>();
        let settings = Settings::load(&db);
        let mut changed = false;

        embed_backlog(&db).await;

        if settings.memory_learning && USER_MESSAGES_SINCE_LEARN.load(Ordering::Relaxed) >= LEARN_EVERY {
            USER_MESSAGES_SINCE_LEARN.store(0, Ordering::Relaxed);
            match learn(&db, &settings).await {
                Ok(n) if n > 0 => {
                    log::info!("Learned {n} new fact(s)");
                    changed = true;
                }
                Ok(_) => {}
                Err(e) => log::warn!("Fact learning failed: {e}"),
            }
        }

        match summarize(&db, &settings).await {
            Ok(true) => changed = true,
            Ok(false) => {}
            Err(e) => log::warn!("Summary failed: {e}"),
        }

        if changed {
            app.emit("memory:changed", ()).ok();
        }
    });
}

/// Embeds messages and facts that don't have an embedding yet (a batch at a time)
async fn embed_backlog(db: &Db) {
    if !embed_ready().await {
        return;
    }
    if let Ok(msgs) = db.unembedded_messages(32) {
        let texts: Vec<String> = msgs.iter().map(|(_, t)| as_document(t)).collect();
        if let Some(embs) = embed_many(texts).await {
            for ((id, _), e) in msgs.iter().zip(embs) {
                db.set_message_embedding(*id, &e).ok();
            }
        }
    }
    if let Ok(facts) = db.facts_with_embeddings() {
        let missing: Vec<&Fact> = facts.iter().filter(|(_, e)| e.is_none()).map(|(f, _)| f).collect();
        let texts: Vec<String> = missing.iter().map(|f| as_document(&f.text)).collect();
        if let Some(embs) = embed_many(texts).await {
            for (f, e) in missing.iter().zip(embs) {
                db.set_fact_embedding(f.id, &e).ok();
            }
        }
    }
}

const LEARN_PROMPT: &str = "You pick out lasting facts about the user from a chat, to remember for weeks. \
Good facts: their name, likes and dislikes, people and pets in their life, work and projects, goals, \
plans with dates, important events, how they feel about something big. \
Skip small talk, passing moods, questions, and anything the assistant said about itself. \
Write each fact as a short sentence in the third person, starting with \"The user\". \
Answer with JSON only: {\"facts\": [\"...\"]}. Use {\"facts\": []} if nothing is worth keeping.";

/// Pulls facts out of the model's JSON answer, whatever shape it took.
pub fn parse_facts(json: &str) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_str(json.trim()) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let list = match &v {
        serde_json::Value::Array(a) => a.clone(),
        serde_json::Value::Object(o) => o
            .get("facts")
            .and_then(|f| f.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => vec![],
    };
    list.iter()
        .filter_map(|item| match item {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(o) => o
                .get("fact")
                .or_else(|| o.get("text"))
                .and_then(|t| t.as_str())
                .map(str::to_string),
            _ => None,
        })
        .map(|s| s.trim().to_string())
        .filter(|s| (8..=200).contains(&s.len()))
        .take(5)
        .collect()
}

async fn learn(db: &Db, settings: &Settings) -> Result<usize, String> {
    let recent = db.recent_messages(LEARN_EVERY * 2)?;
    if !recent.iter().any(|m| m.role == "user") {
        return Ok(0);
    }
    let known = db.facts_with_embeddings()?;
    let known_list = known
        .iter()
        .take(40)
        .map(|(f, _)| format!("- {}", f.text))
        .collect::<Vec<_>>()
        .join("\n");
    let chat = recent
        .iter()
        .map(|m| format!("{}: {}", if m.role == "user" { "User" } else { "Assistant" }, m.text))
        .collect::<Vec<_>>()
        .join("\n");
    let messages = [
        ChatMessage::new("system", LEARN_PROMPT),
        ChatMessage::new(
            "user",
            format!(
                "Already known:\n{}\n\nChat:\n{chat}\n\nList only facts that aren't already known.",
                if known_list.is_empty() { "(nothing yet)".into() } else { known_list }
            ),
        ),
    ];
    let answer = ollama::chat_once(&settings.model, &messages, true).await?;

    let mut added = 0;
    for fact in parse_facts(&answer) {
        if known.iter().any(|(f, _)| f.text.eq_ignore_ascii_case(&fact)) {
            continue;
        }
        let emb = embed_document(&fact).await;
        if let Some(e) = &emb {
            let dup = known
                .iter()
                .any(|(_, k)| k.as_ref().is_some_and(|k| cosine(e, k) >= DUPLICATE_SIM));
            if dup {
                continue;
            }
        }
        db.add_fact(&fact, "auto", emb.as_deref())?;
        added += 1;
    }
    Ok(added)
}

/// Summarizes messages that have scrolled out of the prompt window. Returns
/// whether a summary was written.
async fn summarize(db: &Db, settings: &Settings) -> Result<bool, String> {
    let Some(window_start) = db.nth_newest_message_id(crate::companion::HISTORY_MAX_MESSAGES)? else {
        return Ok(false);
    };
    let after = db.last_summarized_id()?;
    let old = db.messages_between(after, window_start, SUMMARIZE_CHUNK)?;
    if old.len() < SUMMARIZE_MIN {
        return Ok(false);
    }
    let user = if settings.user_name.is_empty() { "the user" } else { settings.user_name.as_str() };
    let chat = old
        .iter()
        .map(|m| format!("{}: {}", if m.role == "user" { "User" } else { "You" }, m.text))
        .collect::<Vec<_>>()
        .join("\n");
    let messages = [
        ChatMessage::new(
            "system",
            format!(
                "You are {}, summarizing part of an earlier chat with {user} for your own memory. \
                 Write 3-5 plain sentences from your point of view (\"we talked about...\"). Keep names, \
                 plans, dates and how they felt. No lists.",
                settings.companion_name
            ),
        ),
        ChatMessage::new("user", chat),
    ];
    let text = ollama::chat_once(&settings.model, &messages, false).await?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(false);
    }
    db.add_summary(old[0].id, old[old.len() - 1].id, text)?;
    log::info!("Summarized messages {}-{}", old[0].id, old[old.len() - 1].id);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(id: i64, text: &str) -> Fact {
        Fact {
            id,
            text: text.into(),
            origin: "auto".into(),
            created: 0,
            last_used: 0,
        }
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn few_facts_all_go_in() {
        let facts = (0..3).map(|i| (fact(i, "x"), None)).collect();
        assert_eq!(pick_facts(facts, None).len(), 3);
    }

    #[test]
    fn many_facts_pick_relevant_then_newest() {
        // 20 facts, newest first; only id 5 points the same way as the query
        let facts: Vec<_> = (0..20)
            .rev()
            .map(|i| (fact(i, "x"), Some(if i == 5 { vec![1.0, 0.0] } else { vec![0.0, 1.0] })))
            .collect();
        let picked = pick_facts(facts, Some(&[1.0, 0.0]));
        assert_eq!(picked.len(), FACTS_TOP);
        assert_eq!(picked[0].id, 5);
        assert_eq!(picked[1].id, 19); // then the newest
    }

    #[test]
    fn parses_remember_and_forget() {
        assert_eq!(
            parse_command("Remember that my sister is Priya."),
            Some(Command::Remember("my sister is Priya".into()))
        );
        assert_eq!(
            parse_command("please remember: exam on friday"),
            Some(Command::Remember("exam on friday".into()))
        );
        assert_eq!(parse_command("forget about my exam"), Some(Command::Forget("my exam".into())));
        assert_eq!(parse_command("do you remember my sister?"), None);
        assert_eq!(parse_command("remember when we talked?"), None);
        assert_eq!(parse_command("forget it"), None);
        assert_eq!(parse_command("I forget things a lot"), None);
    }

    #[test]
    fn parses_facts_in_any_shape() {
        assert_eq!(
            parse_facts(r#"{"facts": ["The user likes momos", "x"]}"#),
            vec!["The user likes momos"]
        );
        assert_eq!(
            parse_facts(r#"[{"fact": "The user has a cat named Mochi"}]"#),
            vec!["The user has a cat named Mochi"]
        );
        assert!(parse_facts("not json").is_empty());
        assert!(parse_facts(r#"{"facts": []}"#).is_empty());
    }
}
