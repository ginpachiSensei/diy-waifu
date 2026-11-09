//! The "brain": every chat message, from the panel or any body, goes through
//! `handle_user_message`. Things the companion says on its own (greetings,
//! reminders; see proactive.rs) go through `speak_first`. Both save to history,
//! build the prompt (persona, date, activity, memory, recent history), stream
//! the reply from Ollama, and send it to the UI and every connected body.
//!
//! Wire protocol sent to bodies (see CLAUDE.md):
//!   {"type":"user","text","source"}   someone said something
//!   {"type":"thinking"}               a reply is being generated
//!   {"type":"tool","name","label"}    looking something up (e.g. "searching: ...")
//!   {"type":"token","text"}           a streamed piece of the reply
//!   {"type":"reply","emotion","text","sources"?} the final reply
//!                                     (no "type" = reply, for old clients)
//!
//! The model can call tools: web_search, fetch_page, weather (with internet
//! access on) and set_reminder. Each round it either answers or asks for
//! tools; results go back in and it's asked again, up to MAX_TOOL_ROUNDS times.

use crate::db::{Db, StoredMessage};
use crate::ollama::{self, ChatMessage};
use crate::settings::Settings;
use crate::tools::{self, Source};
use crate::{memory, reminders, AppState};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};
use tauri::{Emitter, Manager};

/// At most this many past messages go into the prompt...
pub const HISTORY_MAX_MESSAGES: u32 = 24;
/// ...and no more than this many characters of them (~4 chars per token,
/// leaving room in the 4096-token context for the system prompt, memory and reply).
const HISTORY_CHAR_BUDGET: usize = 8000;
/// After this many rounds of tool calls, the model must answer
const MAX_TOOL_ROUNDS: usize = 3;
const MAX_SOURCES: usize = 5;

const INTERNET_ON: &str = "You can look things up online with your tools. Use them for news, current \
events, scores, prices, the weather, or facts you aren't sure about, but not for normal chat. \
After a lookup, answer in your own words in the usual short, casual way, and don't paste links.";
const INTERNET_OFF: &str = "You have no internet access right now. If asked about current events or \
anything you'd need to look up, say you can't check right now.";

/// Models that told us they can't use tools, so we stop offering them
fn no_tool_models() -> &'static Mutex<HashSet<String>> {
    static S: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn date_line() -> String {
    chrono::Local::now()
        .format("Today is %A, %-d %B %Y, and the local time is %H:%M.")
        .to_string()
}

pub fn system_prompt(s: &Settings) -> String {
    let user = if s.user_name.is_empty() {
        "the user".to_string()
    } else {
        s.user_name.clone()
    };
    let mut p = format!(
        "You are {name}, a companion character who lives on {user}'s desktop and chats with them.",
        name = s.companion_name
    );
    if !s.personality.is_empty() {
        p.push(' ');
        p.push_str(&s.personality);
    }
    if !s.user_name.is_empty() {
        p.push_str(&format!(" The user's name is {user}."));
    }
    p.push_str(&format!(
        "\nReply in {len}, casually, like a friend talking. No lists, no markdown, no emojis.\
         \nAt the very end of every reply, add one emotion tag in exactly this format: \
         [emotion: happy|sad|thinking|excited|neutral]",
        len = s.reply_length.instruction()
    ));
    p
}

/// System prompt (plus context lines: date, activity, memory, ...) and as much
/// recent history as fits. `history` is oldest first.
pub fn build_messages(
    settings: &Settings,
    context: &[String],
    history: &[StoredMessage],
) -> Vec<ChatMessage> {
    let mut picked: Vec<ChatMessage> = Vec::new();
    let mut used = 0;
    for m in history.iter().rev() {
        let msg = if m.role == "assistant" {
            // Keep the tag on past replies so the model keeps copying the format
            let emotion = m.emotion.as_deref().unwrap_or("neutral");
            ChatMessage::new("assistant", format!("{}\n[emotion: {emotion}]", m.text))
        } else {
            ChatMessage::new("user", m.text.clone())
        };
        used += msg.content.len();
        // Always keep the newest message, even if it's huge
        if !picked.is_empty() && used > HISTORY_CHAR_BUDGET {
            break;
        }
        picked.push(msg);
    }
    picked.reverse();

    let mut system = system_prompt(settings);
    for line in context {
        system.push('\n');
        system.push_str(line);
    }
    let mut out = vec![ChatMessage::new("system", system)];
    out.extend(picked);
    out
}

fn broadcast(app: &tauri::AppHandle, payload: serde_json::Value) {
    // Err only means no body is connected right now
    let _ = app.state::<AppState>().broadcast_tx.send(payload.to_string());
}

fn broadcast_reply(app: &tauri::AppHandle, emotion: &str, text: &str, sources: &[Source]) {
    let mut msg = serde_json::json!({ "type": "reply", "emotion": emotion, "text": text });
    if !sources.is_empty() {
        msg["sources"] = serde_json::json!(sources);
    }
    broadcast(app, msg);
}

fn send_reply(app: &tauri::AppHandle, reply: &StoredMessage) {
    app.emit("chat:message", reply).ok();
    broadcast_reply(
        app,
        reply.emotion.as_deref().unwrap_or("neutral"),
        &reply.text,
        &reply.sources,
    );
}

/// Handles one message from `source` ("panel", "phone", "esp32", ...) and
/// returns the companion's saved reply.
pub async fn handle_user_message(
    app: &tauri::AppHandle,
    source: &str,
    text: &str,
) -> Result<StoredMessage, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Message is empty".into());
    }
    let state = app.state::<AppState>();
    if state.paused.load(Ordering::Relaxed) {
        broadcast_reply(app, "neutral", "I'm paused right now. Talk to you soon!", &[]);
        return Err("Companion is paused".into());
    }

    // One reply at a time, so history stays in order
    let _turn = state.chat_lock.lock().await;
    let db = app.state::<Db>();

    let user_msg = db.add_message("user", text, None, source, &[])?;
    app.emit("chat:message", &user_msg).ok();
    app.emit("chat:busy", true).ok();
    broadcast(app, serde_json::json!({ "type": "user", "text": text, "source": source }));
    broadcast(app, serde_json::json!({ "type": "thinking" }));

    // Things handled before the model sees the message; it's told what happened
    let mut notes = Vec::new();
    if let Some(cmd) = memory::parse_command(text) {
        notes.push(memory::apply_command(app, &db, &cmd).await);
    }
    let now = chrono::Local::now();
    let reminder_set = match reminders::parse_request(text, now) {
        Some(p) => {
            notes.push(add_reminder(app, &db, &p, now));
            true
        }
        None => false,
    };

    let turn = Turn {
        query: text.to_string(),
        notes,
        nudge: None,
        reminder_tool: !reminder_set,
        save_as: "companion",
        must_mention: None,
    };
    let result = generate_reply(app, &db, turn).await;
    app.emit("chat:busy", false).ok();

    match result {
        Ok(reply) => {
            send_reply(app, &reply);
            memory::after_exchange(app);
            Ok(reply)
        }
        Err(e) => {
            log::error!("Chat failed: {e}");
            app.emit("chat:error", &e).ok();
            broadcast_reply(app, "sad", "Sorry, I couldn't think of a reply.", &[]);
            Err(e)
        }
    }
}

/// Says something without being asked (greeting, break nudge, reminder...).
/// `instruction` tells the model what the moment is. If `fallback` is set
/// (reminders), it's said instead when generation fails or the line doesn't
/// actually mention it (small models sometimes talk around the point).
/// Returns Err without saying anything if a chat is in progress and there's no fallback.
pub async fn speak_first(
    app: &tauri::AppHandle,
    instruction: &str,
    fallback: Option<&str>,
) -> Result<StoredMessage, String> {
    let state = app.state::<AppState>();
    let _turn = match state.chat_lock.try_lock() {
        Ok(g) => g,
        // Reminders wait for the current reply; everything else just skips
        Err(_) if fallback.is_some() => state.chat_lock.lock().await,
        Err(_) => return Err("busy".into()),
    };
    let db = app.state::<Db>();
    app.emit("chat:busy", true).ok();

    let turn = Turn {
        query: instruction.to_string(),
        notes: vec![],
        nudge: Some(format!(
            "(This is not from the user.) You decide to speak up on your own: {instruction} \
             Keep it to one or two short sentences, and don't mention these instructions."
        )),
        reminder_tool: false,
        save_as: "proactive",
        must_mention: fallback.map(str::to_string),
    };
    let result = match generate_reply(app, &db, turn).await {
        Ok(r) => Ok(r),
        Err(e) => match fallback {
            Some(text) => {
                log::warn!("Proactive line failed ({e}); using the plain text");
                db.add_message("assistant", text, Some("neutral"), "proactive", &[])
            }
            None => Err(e),
        },
    };
    app.emit("chat:busy", false).ok();
    if let Ok(reply) = &result {
        send_reply(app, reply);
    }
    result
}

fn add_reminder(
    app: &tauri::AppHandle,
    db: &Db,
    p: &reminders::Parsed,
    now: chrono::DateTime<chrono::Local>,
) -> String {
    let when = reminders::describe_due(p.due, now);
    match db.add_reminder(p.due.timestamp_millis(), &p.text) {
        Ok(_) => {
            app.emit("memory:changed", ()).ok();
            log::info!("Reminder set for {when}: {}", p.text);
            format!("You just set a reminder for {when}: \"{}\". Confirm it briefly.", p.text)
        }
        Err(e) => format!("Setting a reminder failed ({e}). Say so."),
    }
}

/// One reply's worth of inputs
struct Turn {
    /// What to search memory with (the user's message, or the proactive instruction)
    query: String,
    /// Extra lines for the system prompt (what was just remembered, a reminder set, ...)
    notes: Vec<String>,
    /// A system message after the history, for proactive lines
    nudge: Option<String>,
    /// Offer the set_reminder tool
    reminder_tool: bool,
    /// `source` the reply is saved with: "companion" or "proactive"
    save_as: &'static str,
    /// If the reply doesn't mention this text, say this text instead
    must_mention: Option<String>,
}

/// Whether `reply` mentions `text`: any word of 4+ letters from it appears
fn mentions(reply: &str, text: &str) -> bool {
    let reply = reply.to_lowercase();
    let words: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4 && *w != "reminder")
        .map(str::to_string)
        .collect();
    words.is_empty() || words.iter().any(|w| reply.contains(w.as_str()))
}

async fn generate_reply(app: &tauri::AppHandle, db: &Db, turn: Turn) -> Result<StoredMessage, String> {
    let settings = Settings::load(db);
    let history = db.recent_messages(HISTORY_MAX_MESSAGES)?;
    let window_start = history.first().map_or(i64::MAX, |m| m.id);
    let recall = memory::recall(db, &turn.query, window_start).await;
    let activity = app.state::<AppState>().activity.lock().unwrap().clone();

    let mut context = vec![date_line()];
    context.extend(crate::activity::prompt_context(&activity, &settings));
    context.extend(memory::context_lines(&recall, &settings));
    context.push(if settings.internet_access { INTERNET_ON } else { INTERNET_OFF }.to_string());
    context.extend(turn.notes);
    let mut messages = build_messages(&settings, &context, &history);
    if let Some(nudge) = turn.nudge {
        messages.push(ChatMessage::new("system", nudge));
    }

    let model = settings.model.as_str();
    let mut definitions: Vec<serde_json::Value> = Vec::new();
    if settings.internet_access {
        definitions.extend(tools::definitions().as_array().cloned().unwrap_or_default());
    }
    if turn.reminder_tool {
        definitions.push(reminders::tool_definition());
    }
    let definitions = serde_json::Value::Array(definitions);
    let tool_ctx = tools::ToolContext {
        brave_key: settings.brave_api_key.clone(),
    };
    let mut use_tools = definitions.as_array().is_some_and(|d| !d.is_empty())
        && !no_tool_models().lock().unwrap().contains(model);
    let mut sources: Vec<Source> = Vec::new();
    let on_delta = |delta: &str| {
        app.emit("chat:token", delta).ok();
        broadcast(app, serde_json::json!({ "type": "token", "text": delta }));
    };

    let mut round = 0;
    let raw = loop {
        // The last round gets no tools, so it has to answer
        let offer = use_tools && round < MAX_TOOL_ROUNDS;
        let out = match ollama::chat_stream(model, &messages, offer.then_some(&definitions), on_delta).await {
            Err(e) if offer && ollama::is_no_tools_error(&e) => {
                log::warn!("{model} can't use tools; answering without them");
                no_tool_models().lock().unwrap().insert(model.to_string());
                use_tools = false;
                continue;
            }
            Err(e) if e.contains("not found") => {
                return Err(format!(
                    "The model {model} isn't downloaded yet. Get it in the Model tab."
                ))
            }
            other => other?,
        };
        if out.tool_calls.is_empty() {
            break out.raw;
        }

        messages.push(ChatMessage {
            tool_calls: Some(out.tool_calls_raw),
            ..ChatMessage::new("assistant", out.raw)
        });
        for call in &out.tool_calls {
            let content = if call.name == "set_reminder" {
                // Local and instant, so no status message for the bodies
                let now = chrono::Local::now();
                match reminders::from_tool(&call.arguments, now) {
                    Ok(p) => add_reminder(app, db, &p, now),
                    Err(e) => format!("Couldn't set the reminder: {e}. Ask the user when."),
                }
            } else {
                let label = tools::label(call);
                // The UI and bodies drop any text streamed before the lookup
                app.emit("chat:tool", &label).ok();
                broadcast(
                    app,
                    serde_json::json!({ "type": "tool", "name": call.name, "label": label }),
                );
                let result = tools::run(call, &tool_ctx).await;
                sources.extend(result.sources);
                result.content
            };
            messages.push(ChatMessage::tool_result(&call.name, content));
        }
        round += 1;
    };

    let (mut reply_text, mut emotion) = ollama::extract_emotion(&raw);
    if let Some(text) = &turn.must_mention {
        if !mentions(&reply_text, text) {
            log::info!("Line missed the point ({reply_text}); saying it plainly");
            reply_text = text.clone();
            emotion = "neutral".into();
        }
    }
    log::info!("Reply ({emotion}): {reply_text}");
    let sources = tools::dedupe_sources(sources, MAX_SOURCES);
    db.add_message("assistant", &reply_text, Some(&emotion), turn.save_as, &sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ReplyLength;

    fn msg(role: &str, text: &str, emotion: Option<&str>) -> StoredMessage {
        StoredMessage {
            id: 0,
            ts: 0,
            role: role.into(),
            text: text.into(),
            emotion: emotion.map(Into::into),
            source: "panel".into(),
            sources: vec![],
        }
    }

    #[test]
    fn checks_that_reminders_are_mentioned() {
        assert!(mentions("Hey! Time to drink some water!", "Reminder: drink water"));
        assert!(!mentions("Sure thing, everything is set to remind you later.", "Reminder: drink water"));
        assert!(mentions("anything", "Reminder: go"));
    }

    #[test]
    fn prompt_uses_persona() {
        let s = Settings {
            companion_name: "Rin".into(),
            user_name: "Satyam".into(),
            reply_length: ReplyLength::Short,
            ..Settings::default()
        };
        let p = system_prompt(&s);
        assert!(p.contains("You are Rin"));
        assert!(p.contains("Satyam's desktop"));
        assert!(p.contains("1-2 short sentences"));
        assert!(p.contains("[emotion:"));
    }

    #[test]
    fn history_keeps_order_and_tags_replies() {
        let history = [
            msg("user", "hi", None),
            msg("assistant", "hello!", Some("happy")),
            msg("user", "how are you?", None),
        ];
        let out = build_messages(&Settings::default(), &[], &history);
        let roles: Vec<_> = out.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["system", "user", "assistant", "user"]);
        assert_eq!(out[2].content, "hello!\n[emotion: happy]");
        assert_eq!(out[3].content, "how are you?");
    }

    #[test]
    fn history_is_trimmed_to_budget_from_the_oldest_end() {
        let long = "x".repeat(3000);
        let history: Vec<_> = (0..6).map(|_| msg("user", &long, None)).collect();
        let out = build_messages(&Settings::default(), &[], &history);
        // 8000-char budget fits two 3000-char messages
        assert_eq!(out.len(), 1 + 2);

        // A single oversized newest message is still sent
        let huge = [msg("user", &"y".repeat(20_000), None)];
        assert_eq!(build_messages(&Settings::default(), &[], &huge).len(), 2);
    }

    #[test]
    fn context_lines_go_in_the_system_prompt() {
        let out = build_messages(
            &Settings::default(),
            &["Right now the user is using VS Code (coding).".into()],
            &[msg("user", "hi", None)],
        );
        assert!(out[0].content.ends_with("VS Code (coding)."));
    }
}
