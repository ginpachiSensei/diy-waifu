use serde::{Deserialize, Serialize};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

pub struct OllamaHandle {
    pub child: Option<CommandChild>,
}

// Spawn the bundled ollama.exe sidecar as `ollama serve`
pub fn spawn_ollama(app_handle: &tauri::AppHandle) -> Result<CommandChild, String> {
    let sidecar = app_handle
        .shell()
        .sidecar("ollama")
        .map_err(|e| e.to_string())?
        .args(["serve"]);

    let (mut rx, child) = sidecar.spawn().map_err(|e| e.to_string())?;

    // Drain sidecar stdout/stderr so it doesn't block, log for debugging
    tauri::async_runtime::spawn(async move {
        use tauri_plugin_shell::process::CommandEvent;
        while let Some(event) = rx.recv().await {
            if let CommandEvent::Stdout(line) = &event {
                println!("[ollama] {}", String::from_utf8_lossy(line));
            }
            if let CommandEvent::Stderr(line) = &event {
                eprintln!("[ollama:err] {}", String::from_utf8_lossy(line));
            }
        }
    });

    Ok(child)
}

// Poll /api/tags until Ollama responds or we give up
pub async fn wait_for_ollama_ready(max_attempts: u32) -> bool {
    let client = reqwest::Client::new();
    for attempt in 1..=max_attempts {
        match client.get("http://localhost:11434/api/tags").send().await {
            Ok(resp) if resp.status().is_success() => {
                println!("Ollama ready after {} attempt(s)", attempt);
                return true;
            }
            _ => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
    false
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

const SYSTEM_PROMPT: &str = "You are a cheerful anime companion character. \
Keep replies short (1-2 sentences). \
At the very end of every reply, append an emotion tag on its own line in the \
exact format: [emotion: happy|sad|thinking|excited|neutral]";

// Calls Ollama, returns (reply_text, emotion)
pub async fn chat(user_text: &str, model: &str) -> Result<(String, String), String> {
    let client = reqwest::Client::new();
    let req = ChatRequest {
        model,
        messages: vec![
            ChatMessage { role: "system", content: SYSTEM_PROMPT },
            ChatMessage { role: "user", content: user_text },
        ],
        stream: false,
    };

    let resp = client
        .post("http://localhost:11434/api/chat")
        .json(&req)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let parsed: ChatResponse = resp.json().await.map_err(|e| e.to_string())?;
    let raw = parsed.message.content;

    // Extract "[emotion: xxx]" and strip it from the displayed text
    let (text, emotion) = extract_emotion(&raw);
    Ok((text, emotion))
}

fn extract_emotion(raw: &str) -> (String, String) {
    if let Some(start) = raw.rfind("[emotion:") {
        let tag_part = &raw[start..];
        let emotion = tag_part
            .trim_start_matches("[emotion:")
            .trim_end_matches(']')
            .trim()
            .to_lowercase();
        let text = raw[..start].trim().to_string();
        (text, emotion)
    } else {
        (raw.trim().to_string(), "neutral".to_string())
    }
}
