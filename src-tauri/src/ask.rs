//! "Ask me first" for internet lookups (`settings.ask_before_lookup`).
//!
//! Before a tool goes online, the question goes to the panel (`chat:ask`
//! event) and every body (`{"type":"ask","id","text"}`). Whichever answers
//! first wins: the panel and pet through `answer_ask`, other bodies with
//! `{"type":"answer","id","yes"}`. No answer within `TIMEOUT` counts as no.
//! When it's settled, `chat:ask_done` / `{"type":"ask_done","id"}` tells the
//! others to hide the question.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Emitter as _, Manager as _};
use tokio::sync::oneshot;

const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Asks {
    next: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
}

#[derive(Clone, serde::Serialize)]
pub struct Question {
    pub id: u64,
    pub text: String,
}

/// Asks the user and waits for yes/no.
pub async fn ask(app: &tauri::AppHandle, text: String) -> bool {
    let asks = app.state::<Asks>();
    let id = asks.next.fetch_add(1, Ordering::Relaxed) + 1;
    let (tx, rx) = oneshot::channel();
    asks.pending.lock().unwrap().insert(id, tx);

    log::info!("Asking before lookup: {text}");
    app.emit("chat:ask", Question { id, text: text.clone() }).ok();
    crate::companion::broadcast(app, serde_json::json!({ "type": "ask", "id": id, "text": text }));

    let yes = matches!(tokio::time::timeout(TIMEOUT, rx).await, Ok(Ok(true)));
    asks.pending.lock().unwrap().remove(&id);
    app.emit("chat:ask_done", id).ok();
    crate::companion::broadcast(app, serde_json::json!({ "type": "ask_done", "id": id }));
    log::info!("Lookup {}", if yes { "allowed" } else { "declined" });
    yes
}

/// Answers question `id`. False if it was already answered or timed out.
pub fn answer(app: &tauri::AppHandle, id: u64, yes: bool) -> bool {
    let tx = app.state::<Asks>().pending.lock().unwrap().remove(&id);
    tx.is_some_and(|tx| tx.send(yes).is_ok())
}
