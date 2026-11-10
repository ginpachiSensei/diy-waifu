use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;
#[cfg(not(windows))]
use tauri::Manager as _;

use crate::tools::ToolCall;

const OLLAMA_URL: &str = "http://localhost:11434";

/// Context window we ask Ollama for. History is trimmed to fit well inside it.
const NUM_CTX: u32 = 4096;
/// How long Ollama keeps the model in memory after a reply. Longer means fewer
/// slow "cold" replies, at the cost of holding RAM while idle.
const KEEP_ALIVE: &str = "30m";

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

// ---------------------------------------------------------------------------
// Sidecar process
// ---------------------------------------------------------------------------

// Spawn the bundled ollama.exe sidecar as `ollama serve`
pub fn spawn_ollama(app_handle: &tauri::AppHandle) -> Result<CommandChild, String> {
    let sidecar = app_handle
        .shell()
        .sidecar("ollama")
        .map_err(|e| e.to_string())?
        .args(["serve"]);
    // macOS/Linux bundles keep resources away from the exe, so Ollama finds its runner
    // through <working dir>/build/lib/ollama instead (see setup-ollama.cjs)
    #[cfg(not(windows))]
    let sidecar = match app_handle.path().resource_dir() {
        Ok(dir) if dir.join("ollama-runtime").is_dir() => sidecar.current_dir(dir.join("ollama-runtime")),
        _ => sidecar,
    };

    let (mut rx, child) = sidecar.spawn().map_err(|e| e.to_string())?;

    // Drain sidecar stdout/stderr so it doesn't block, log for debugging
    tauri::async_runtime::spawn(async move {
        use tauri_plugin_shell::process::CommandEvent;
        while let Some(event) = rx.recv().await {
            match &event {
                CommandEvent::Stdout(line) => {
                    log::info!("[ollama] {}", String::from_utf8_lossy(line).trim_end())
                }
                // Ollama writes its normal logs to stderr
                CommandEvent::Stderr(line) => {
                    log::info!("[ollama] {}", String::from_utf8_lossy(line).trim_end())
                }
                CommandEvent::Terminated(status) => {
                    log::warn!("Ollama sidecar exited: {:?}", status)
                }
                _ => {}
            }
        }
    });

    Ok(child)
}

// Poll /api/tags until Ollama responds or we give up
pub async fn wait_for_ollama_ready(max_attempts: u32) -> bool {
    for attempt in 1..=max_attempts {
        match client().get(format!("{OLLAMA_URL}/api/tags")).send().await {
            Ok(resp) if resp.status().is_success() => {
                log::info!("Ollama ready after {} attempt(s)", attempt);
                return true;
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
        }
    }
    false
}

/// Quick "is Ollama answering" check for the watchdog.
pub async fn is_healthy() -> bool {
    client()
        .get(format!("{OLLAMA_URL}/api/tags"))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Helpers for Ollama's streaming (newline-delimited JSON) responses
// ---------------------------------------------------------------------------

/// Turns a non-2xx response into an error, using Ollama's `{"error": ...}` body if present.
async fn check(resp: reqwest::Response) -> Result<reqwest::Response, String> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_string))
        .unwrap_or(body);
    Err(format!("Ollama returned {status}: {msg}"))
}

/// Calls `f` with each JSON line of a streamed response. Network chunks don't
/// line up with lines, so bytes are buffered until a newline arrives.
/// `f` returns `Ok(false)` to stop early.
async fn for_each_json_line<T: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
    mut f: impl FnMut(T) -> Result<bool, String>,
) -> Result<(), String> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut handle = |line: &[u8]| -> Result<bool, String> {
        let line = line.trim_ascii();
        if line.is_empty() {
            return Ok(true);
        }
        match serde_json::from_slice::<T>(line) {
            Ok(item) => f(item),
            Err(e) => {
                log::warn!("Skipping unreadable Ollama line ({e}): {}", String::from_utf8_lossy(line));
                Ok(true)
            }
        }
    };
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            if !handle(&line)? {
                return Ok(());
            }
        }
    }
    handle(&buf)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Model management
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullProgress {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub completed: Option<u64>,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default, skip_serializing)]
    error: Option<String>,
}

/// Downloads a model, calling `on_progress` for each update Ollama sends.
pub async fn pull_model(
    model: &str,
    mut on_progress: impl FnMut(&PullProgress),
) -> Result<(), String> {
    let resp = client()
        .post(format!("{OLLAMA_URL}/api/pull"))
        .json(&serde_json::json!({ "model": model, "stream": true }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let resp = check(resp).await?;

    let mut succeeded = false;
    for_each_json_line::<PullProgress>(resp, |p| {
        if let Some(e) = &p.error {
            return Err(e.clone());
        }
        on_progress(&p);
        if p.status == "success" {
            succeeded = true;
            return Ok(false);
        }
        Ok(true)
    })
    .await?;

    if succeeded {
        Ok(())
    } else {
        Err("Download stopped before it finished".into())
    }
}

// Remove/uninstall the model to free disk space
pub async fn remove_model(model: &str) -> Result<(), String> {
    let resp = client()
        .delete(format!("{OLLAMA_URL}/api/delete"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    check(resp).await.map(|_| ())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModel {
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<InstalledModel>,
}

pub async fn list_models() -> Result<Vec<InstalledModel>, String> {
    let resp = client()
        .get(format!("{OLLAMA_URL}/api/tags"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let tags: TagsResponse = check(resp).await?.json().await.map_err(|e| e.to_string())?;
    Ok(tags.models)
}

// Force-unload the model from memory without deleting it (keep_alive: 0)
pub async fn unload_model(model: &str) -> Result<(), String> {
    let resp = client()
        .post(format!("{OLLAMA_URL}/api/generate"))
        .json(&serde_json::json!({ "model": model, "keep_alive": 0 }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    check(resp).await.map(|_| ())
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// On an assistant message that asked for tools, sent back as-is
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    /// On a `role: "tool"` message: which tool this is the result of
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: content.into(),
            tool_calls: None,
            tool_name: None,
        }
    }

    pub fn tool_result(name: &str, content: impl Into<String>) -> Self {
        Self {
            tool_name: Some(name.to_string()),
            ..Self::new("tool", content)
        }
    }
}

#[derive(Deserialize)]
struct ChatChunk {
    #[serde(default)]
    message: Option<ChunkMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct ChunkMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<serde_json::Value>,
}

pub struct ChatOutput {
    /// The reply text, emotion tag included
    pub raw: String,
    /// Tools the model wants run; if not empty, `raw` is usually empty
    pub tool_calls: Vec<ToolCall>,
    /// The same calls in Ollama's format, to send back with the results
    pub tool_calls_raw: Vec<serde_json::Value>,
}

/// `{"function": {"name", "arguments"}}`; arguments may arrive as a JSON string.
fn parse_tool_call(v: &serde_json::Value) -> Option<ToolCall> {
    let f = v.get("function")?;
    let name = f.get("name")?.as_str()?.to_string();
    let arguments = match f.get("arguments") {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
        Some(a) => a.clone(),
        None => serde_json::Value::Null,
    };
    Some(ToolCall { name, arguments })
}

/// Streams a chat reply. `on_delta` gets each new piece of the reply as it
/// arrives, with any trailing emotion tag held back (see `visible_stream_text`).
/// With `tools`, the model may answer with tool calls instead of text.
pub async fn chat_stream(
    model: &str,
    messages: &[ChatMessage],
    tools: Option<&serde_json::Value>,
    mut on_delta: impl FnMut(&str),
) -> Result<ChatOutput, String> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "keep_alive": KEEP_ALIVE,
        "options": { "num_ctx": NUM_CTX },
    });
    if let Some(t) = tools {
        body["tools"] = t.clone();
    }
    let resp = client()
        .post(format!("{OLLAMA_URL}/api/chat"))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let resp = check(resp).await?;

    let mut raw = String::new();
    let mut tool_calls_raw = Vec::new();
    let mut sent = 0;
    for_each_json_line::<ChatChunk>(resp, |chunk| {
        if let Some(e) = chunk.error {
            return Err(e);
        }
        if let Some(m) = chunk.message {
            tool_calls_raw.extend(m.tool_calls);
            raw.push_str(&m.content);
            let visible = visible_stream_text(&raw);
            if visible.len() > sent {
                on_delta(&visible[sent..]);
                sent = visible.len();
            }
        }
        Ok(!chunk.done)
    })
    .await?;

    let tool_calls: Vec<ToolCall> = tool_calls_raw.iter().filter_map(parse_tool_call).collect();
    if raw.trim().is_empty() && tool_calls.is_empty() {
        return Err("The model sent an empty reply".into());
    }
    Ok(ChatOutput {
        raw,
        tool_calls,
        tool_calls_raw,
    })
}

/// One non-streamed reply, for background jobs (fact extraction, summaries).
/// With `json`, Ollama constrains the output to valid JSON.
pub async fn chat_once(model: &str, messages: &[ChatMessage], json: bool) -> Result<String, String> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
        "keep_alive": KEEP_ALIVE,
        "options": { "num_ctx": NUM_CTX, "temperature": 0.2 },
    });
    if json {
        body["format"] = "json".into();
    }
    let resp = client()
        .post(format!("{OLLAMA_URL}/api/chat"))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = check(resp).await?.json().await.map_err(|e| e.to_string())?;
    Ok(v["message"]["content"].as_str().unwrap_or_default().to_string())
}

/// Embeddings for `inputs`, one vector each, in order.
pub async fn embed(model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
    #[derive(Deserialize)]
    struct EmbedResponse {
        embeddings: Vec<Vec<f32>>,
    }
    let resp = client()
        .post(format!("{OLLAMA_URL}/api/embed"))
        .json(&serde_json::json!({ "model": model, "input": inputs, "keep_alive": KEEP_ALIVE }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let r: EmbedResponse = check(resp).await?.json().await.map_err(|e| e.to_string())?;
    if r.embeddings.len() != inputs.len() {
        return Err("Ollama returned the wrong number of embeddings".into());
    }
    Ok(r.embeddings)
}

/// Ollama's error for models without tool support
pub fn is_no_tools_error(e: &str) -> bool {
    e.contains("does not support tools")
}

/// The part of a partly streamed reply that's safe to show. Text from the last
/// `[` onward is held back while it could still be the emotion tag (no `]` yet,
/// or a complete `[...]` at the very end). The cut point only moves forward,
/// so the shown text never shrinks as more arrives.
pub fn visible_stream_text(raw: &str) -> &str {
    let Some(start) = raw.rfind('[') else {
        return raw;
    };
    match raw[start..].find(']') {
        None => &raw[..start],
        Some(end) if raw[start + end + 1..].trim().is_empty() => &raw[..start],
        Some(_) => raw,
    }
}

const EMOTIONS: &[&str] = &["happy", "sad", "thinking", "excited", "neutral"];

// Small models don't always follow the exact "[emotion: x]" format, so accept the last
// bracketed tag at the end of the reply, with or without the "emotion:" prefix.
pub fn extract_emotion(raw: &str) -> (String, String) {
    let trimmed = raw.trim();
    if trimmed.ends_with(']') {
        if let Some(start) = trimmed.rfind('[') {
            let tag = trimmed[start + 1..trimmed.len() - 1].trim().to_lowercase();
            let tag = tag.strip_prefix("emotion:").unwrap_or(&tag).trim();
            if let Some(emotion) = EMOTIONS.iter().find(|e| tag == **e) {
                return (trimmed[..start].trim().to_string(), emotion.to_string());
            }
        }
    }
    (trimmed.to_string(), "neutral".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_emotion_tag_variants() {
        let cases = [
            ("Hi there!\n[emotion: happy]", "Hi there!", "happy"),
            ("Hi there! [happy]", "Hi there!", "happy"),
            ("Hmm... [Emotion:Thinking]", "Hmm...", "thinking"),
            ("No tag here", "No tag here", "neutral"),
            ("Unknown [angry]", "Unknown [angry]", "neutral"),
        ];
        for (raw, text, emotion) in cases {
            assert_eq!(extract_emotion(raw), (text.to_string(), emotion.to_string()), "{raw}");
        }
    }

    /// Needs Ollama running with the default model:
    /// `cargo test live_chat_streams -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_chat_streams() {
        let messages = [
            ChatMessage::new("system", "Reply in one sentence, then add [emotion: happy|sad|neutral]."),
            ChatMessage::new("user", "Say hi!"),
        ];
        let mut deltas = String::new();
        let raw = chat_stream(crate::settings::DEFAULT_MODEL, &messages, None, |d| deltas.push_str(d))
            .await
            .expect("chat failed")
            .raw;
        println!("raw: {raw:?}\nstreamed: {deltas:?}");
        let (text, _) = extract_emotion(&raw);
        assert_eq!(deltas.trim(), text.trim());
    }

    #[test]
    fn parses_tool_calls_in_both_argument_formats() {
        let obj = serde_json::json!({"function": {"name": "web_search", "arguments": {"query": "x"}}});
        let s = serde_json::json!({"function": {"name": "weather", "arguments": "{\"place\":\"Delhi\"}"}});
        assert_eq!(parse_tool_call(&obj).unwrap().arguments["query"], "x");
        assert_eq!(parse_tool_call(&s).unwrap().arguments["place"], "Delhi");
        assert!(parse_tool_call(&serde_json::json!({"nope": 1})).is_none());
    }

    #[test]
    fn tool_messages_serialize_like_ollama_expects() {
        let m = serde_json::to_value(ChatMessage::tool_result("weather", "sunny")).unwrap();
        assert_eq!(m, serde_json::json!({"role": "tool", "content": "sunny", "tool_name": "weather"}));
        let plain = serde_json::to_value(ChatMessage::new("user", "hi")).unwrap();
        assert_eq!(plain, serde_json::json!({"role": "user", "content": "hi"}));
    }

    #[test]
    fn streaming_hides_emotion_tag_and_never_shrinks() {
        let full = "Hi [there] friend!\n[emotion: happy]";
        let mut last = 0;
        for end in 0..=full.len() {
            let v = visible_stream_text(&full[..end]);
            assert!(v.len() >= last, "shrank at {end}: {v:?}");
            last = v.len();
        }
        assert_eq!(visible_stream_text(full).trim(), "Hi [there] friend!");
        assert_eq!(visible_stream_text("Hello [emo"), "Hello ");
        assert_eq!(visible_stream_text("plain"), "plain");
    }
}
