//! Web search through official APIs:
//! - **Brave Search API** when the user has added a key (full web results;
//!   free tier at https://brave.com/search/api).
//! - **DuckDuckGo Instant Answer API** otherwise (no key; quick facts and
//!   Wikipedia-style summaries only, no news or full results).

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_RESULTS: usize = 5;
const CACHE_FOR: Duration = Duration::from_secs(10 * 60);
/// Brave's free tier allows 1 request per second
const MIN_GAP: Duration = Duration::from_millis(1100);

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    /// "Brave Search" or "DuckDuckGo Instant Answers"
    pub provider: &'static str,
    /// True for the keyless fallback, which only knows quick facts
    pub limited: bool,
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Brave puts <strong> around matched words; drop tags and decode the few
/// entities it uses.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    collapse(
        &out.replace("&amp;", "&")
            .replace("&quot;", "\"")
            .replace("&#x27;", "'")
            .replace("&#39;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">"),
    )
}

// ---- Brave ----

#[derive(Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

pub fn parse_brave(json: &str) -> Result<Vec<SearchResult>, String> {
    let r: BraveResponse =
        serde_json::from_str(json).map_err(|e| format!("Unexpected Brave response: {e}"))?;
    Ok(r.web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.starts_with("http"))
        .take(MAX_RESULTS)
        .map(|r| SearchResult {
            title: strip_tags(&r.title),
            url: r.url,
            snippet: strip_tags(&r.description),
        })
        .collect())
}

async fn brave(query: &str, key: &str) -> Result<Vec<SearchResult>, String> {
    let resp = client()
        .get("https://api.search.brave.com/res/v1/web/search")
        .query(&[("q", query), ("count", &MAX_RESULTS.to_string())])
        .header("Accept", "application/json")
        .header("X-Subscription-Token", key)
        .send()
        .await
        .map_err(|e| format!("Search failed: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("Search failed: {e}"))?;
    match status.as_u16() {
        200 => parse_brave(&body),
        401 | 403 => Err("The Brave Search API key was rejected. Check it in the System tab.".into()),
        429 => Err("The Brave Search limit was reached (too fast, or the monthly quota is used up).".into()),
        _ => Err(format!("Brave Search returned {status}")),
    }
}

// ---- DuckDuckGo Instant Answers ----

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct DdgAnswer {
    heading: String,
    abstract_text: String,
    abstract_source: String,
    #[serde(rename = "AbstractURL")]
    abstract_url: String,
    answer: String,
    definition: String,
    definition_source: String,
    #[serde(rename = "DefinitionURL")]
    definition_url: String,
    related_topics: Vec<DdgTopic>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct DdgTopic {
    text: String,
    #[serde(rename = "FirstURL")]
    first_url: String,
    /// Grouped topics nest one level
    topics: Vec<DdgTopic>,
}

pub fn parse_ddg(json: &str) -> Result<Vec<SearchResult>, String> {
    // DDG sometimes returns an empty body for queries it has nothing for
    if json.trim().is_empty() {
        return Ok(vec![]);
    }
    let a: DdgAnswer =
        serde_json::from_str(json).map_err(|e| format!("Unexpected DuckDuckGo response: {e}"))?;
    let mut out = Vec::new();
    let heading = if a.heading.is_empty() {
        "Answer".to_string()
    } else {
        a.heading.clone()
    };
    if !a.answer.is_empty() {
        out.push(SearchResult {
            title: heading.clone(),
            url: "https://duckduckgo.com/".into(),
            snippet: strip_tags(&a.answer),
        });
    }
    if !a.abstract_text.is_empty() {
        out.push(SearchResult {
            title: format!("{heading} ({})", a.abstract_source),
            url: a.abstract_url.clone(),
            snippet: strip_tags(&a.abstract_text),
        });
    }
    if !a.definition.is_empty() {
        out.push(SearchResult {
            title: format!("Definition ({})", a.definition_source),
            url: a.definition_url.clone(),
            snippet: strip_tags(&a.definition),
        });
    }
    let topics = a.related_topics.iter().flat_map(|t| {
        if t.topics.is_empty() {
            vec![t]
        } else {
            t.topics.iter().collect()
        }
    });
    for t in topics {
        if out.len() >= MAX_RESULTS {
            break;
        }
        if !t.text.is_empty() && t.first_url.starts_with("http") {
            out.push(SearchResult {
                title: t.text.split(" - ").next().unwrap_or(&t.text).to_string(),
                url: t.first_url.clone(),
                snippet: strip_tags(&t.text),
            });
        }
    }
    out.retain(|r| r.url.starts_with("http"));
    Ok(out)
}

async fn ddg(query: &str) -> Result<Vec<SearchResult>, String> {
    let resp = client()
        .get("https://api.duckduckgo.com/")
        .query(&[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
            ("t", "diy-waifu"),
        ])
        .send()
        .await
        .map_err(|e| format!("Search failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("DuckDuckGo returned {}", resp.status()));
    }
    parse_ddg(&resp.text().await.map_err(|e| format!("Search failed: {e}"))?)
}

// ---- Shared ----

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("diy-waifu/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("http client")
    })
}

type Cache = HashMap<String, (Instant, SearchOutcome)>;

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Waits so searches are at least MIN_GAP apart, even if several run at once.
async fn wait_turn() {
    static NEXT: OnceLock<tokio::sync::Mutex<Instant>> = OnceLock::new();
    let mut next = NEXT
        .get_or_init(|| tokio::sync::Mutex::new(Instant::now()))
        .lock()
        .await;
    let now = Instant::now();
    if *next > now {
        tokio::time::sleep(*next - now).await;
    }
    *next = Instant::now() + MIN_GAP;
}

/// Searches with Brave if `brave_key` is set, otherwise DuckDuckGo Instant Answers.
pub async fn search(query: &str, brave_key: &str) -> Result<SearchOutcome, String> {
    let query = collapse(query);
    if query.is_empty() {
        return Err("Empty search".into());
    }
    let brave_key = brave_key.trim();
    let cache_key = format!("{}|{}", !brave_key.is_empty(), query.to_lowercase());
    if let Some((at, outcome)) = cache().lock().unwrap().get(&cache_key) {
        if at.elapsed() < CACHE_FOR {
            return Ok(outcome.clone());
        }
    }

    wait_turn().await;
    let outcome = if brave_key.is_empty() {
        SearchOutcome {
            results: ddg(&query).await?,
            provider: "DuckDuckGo Instant Answers",
            limited: true,
        }
    } else {
        SearchOutcome {
            results: brave(&query, brave_key).await?,
            provider: "Brave Search",
            limited: false,
        }
    };

    let mut c = cache().lock().unwrap();
    c.retain(|_, (at, _)| at.elapsed() < CACHE_FOR);
    c.insert(cache_key, (Instant::now(), outcome.clone()));
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_brave_results() {
        let json = r#"{"type":"search","web":{"type":"search","results":[
            {"title":"Rust Programming Language","url":"https://rust-lang.org/",
             "description":"A <strong>language</strong> empowering everyone &amp; more."},
            {"title":"Bad","url":"ftp://x","description":""},
            {"title":"Rust (programming language) - Wikipedia","url":"https://en.wikipedia.org/wiki/Rust_(programming_language)",
             "description":"Rust is a general-purpose programming language."}]}}"#;
        let r = parse_brave(json).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].snippet, "A language empowering everyone & more.");
        assert_eq!(r[1].url, "https://en.wikipedia.org/wiki/Rust_(programming_language)");
        // No "web" section (nothing found) is just no results
        assert!(parse_brave(r#"{"type":"search"}"#).unwrap().is_empty());
    }

    #[test]
    fn parses_ddg_instant_answers() {
        let json = r#"{"Heading":"Rust (programming language)","AbstractText":"Rust is a language.",
            "AbstractSource":"Wikipedia","AbstractURL":"https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "Answer":"","Definition":"","DefinitionSource":"","DefinitionURL":"",
            "RelatedTopics":[
              {"Text":"Cargo - Rust's package manager","FirstURL":"https://duckduckgo.com/Cargo"},
              {"Name":"See also","Topics":[{"Text":"Go - another language","FirstURL":"https://duckduckgo.com/Go"}]}]}"#;
        let r = parse_ddg(json).unwrap();
        assert_eq!(r[0].title, "Rust (programming language) (Wikipedia)");
        assert_eq!(r[0].snippet, "Rust is a language.");
        assert_eq!(r[1].title, "Cargo");
        assert_eq!(r[2].title, "Go");
        assert!(parse_ddg("{}").unwrap().is_empty());
        assert!(parse_ddg("").unwrap().is_empty());
    }

    #[test]
    fn strips_tags_and_entities() {
        assert_eq!(strip_tags("a <b>bold</b>  &quot;x&quot;"), "a bold \"x\"");
    }

    /// Hits the real DuckDuckGo Instant Answer API:
    /// `cargo test live_ddg_answers -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_ddg_answers() {
        let r = search("rust programming language", "").await.unwrap();
        println!("{r:#?}");
        assert!(!r.results.is_empty());
        assert!(r.limited);
    }
}
