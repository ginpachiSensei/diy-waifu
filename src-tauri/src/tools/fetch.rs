//! Fetches a web page and pulls out its readable text. Every hop (including
//! redirects) goes through the SSRF guard, and the request is pinned to the
//! address the guard checked.

use super::guard::resolve_public;
use futures::StreamExt;
use scraper::{ElementRef, Html, Selector};
use std::time::Duration;
use url::Url;

const MAX_BYTES: usize = 2_000_000;
const MAX_TEXT_CHARS: usize = 4000;
const MAX_REDIRECTS: usize = 5;

/// Text inside these is page furniture, not content
const SKIP: &[&str] = &[
    "script", "style", "noscript", "nav", "header", "footer", "aside", "form", "svg", "button", "template",
];

#[derive(Debug, Clone)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub text: String,
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn in_skipped(el: ElementRef) -> bool {
    el.ancestors()
        .filter_map(ElementRef::wrap)
        .chain(std::iter::once(el))
        .any(|a| SKIP.contains(&a.value().name()))
}

/// (title, readable text) from an HTML page.
pub fn extract_text(html: &str) -> (String, String) {
    let doc = Html::parse_document(html);
    let title = Selector::parse("title")
        .ok()
        .and_then(|s| doc.select(&s).next())
        .map(|t| collapse(&t.text().collect::<String>()))
        .unwrap_or_default();

    // Prefer the main content area if the page marks one
    let root = ["article", "main", "[role=main]", "body"]
        .iter()
        .filter_map(|s| Selector::parse(s).ok())
        .find_map(|s| doc.select(&s).next());
    let Some(root) = root else {
        return (title, String::new());
    };

    let blocks = Selector::parse("h1, h2, h3, h4, p, li, pre, blockquote, td, dd").unwrap();
    let mut lines: Vec<String> = Vec::new();
    for el in root.select(&blocks) {
        // Nested blocks (a <p> inside an <li>) would repeat text; keep the innermost
        if el.select(&blocks).nth(1).is_some() || in_skipped(el) {
            continue;
        }
        let line = collapse(&el.text().collect::<String>());
        if line.len() > 1 && lines.last() != Some(&line) {
            lines.push(line);
        }
    }
    let mut text = lines.join("\n");
    if text.trim().is_empty() {
        // No block structure (or a JS app shell): fall back to all visible text
        text = collapse(&root.text().collect::<String>());
    }
    (title, truncate_chars(&text, MAX_TEXT_CHARS))
}

pub async fn fetch_page(url: &str) -> Result<Page, String> {
    let mut url = Url::parse(url.trim()).map_err(|_| format!("Not a valid link: {url}"))?;

    for _ in 0..=MAX_REDIRECTS {
        let addr = resolve_public(&url).await?;
        let host = url.host_str().unwrap_or_default().to_string();
        let client = reqwest::Client::builder()
            .user_agent(super::USER_AGENT)
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .resolve(&host, addr)
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(url.clone())
            .header("Accept", "text/html,text/plain;q=0.9,*/*;q=0.1")
            .send()
            .await
            .map_err(|e| format!("Couldn't open {url}: {e}"))?;

        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or("Redirect without a location")?;
            url = url.join(loc).map_err(|_| "Bad redirect".to_string())?;
            continue;
        }
        if !status.is_success() {
            return Err(format!("{url} returned {status}"));
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        let is_html = content_type.contains("html") || content_type.is_empty();
        if !is_html && !content_type.starts_with("text/") {
            return Err(format!("{url} isn't a web page ({content_type})"));
        }

        let mut body: Vec<u8> = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            let room = MAX_BYTES - body.len();
            body.extend_from_slice(&chunk[..chunk.len().min(room)]);
            if body.len() >= MAX_BYTES {
                break;
            }
        }
        let raw = String::from_utf8_lossy(&body);
        let (title, text) = if is_html {
            extract_text(&raw)
        } else {
            (String::new(), truncate_chars(raw.trim(), MAX_TEXT_CHARS))
        };
        return Ok(Page {
            url: url.to_string(),
            title,
            text,
        });
    }
    Err("Too many redirects".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_article_text_without_furniture() {
        let html = r#"<html><head><title> My  Post </title><script>var x = "nope";</script></head>
            <body><nav><ul><li>Home</li><li>About</li></ul></nav>
            <article><h1>Big news</h1><p>First   paragraph.</p>
              <ul><li><p>Nested item</p></li></ul>
              <aside><p>Related links</p></aside>
            </article><footer><p>Copyright</p></footer></body></html>"#;
        let (title, text) = extract_text(html);
        assert_eq!(title, "My Post");
        assert_eq!(text, "Big news\nFirst paragraph.\nNested item");
    }

    #[test]
    fn falls_back_to_body_text() {
        let (_, text) = extract_text("<html><body><div>Just a div</div></body></html>");
        assert_eq!(text, "Just a div");
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate_chars("héllo", 2), "hé…");
        assert_eq!(truncate_chars("hi", 5), "hi");
    }

    #[tokio::test]
    async fn refuses_local_targets() {
        assert!(fetch_page("http://127.0.0.1:11434/api/tags").await.is_err());
        assert!(fetch_page("not a url").await.is_err());
    }
}
