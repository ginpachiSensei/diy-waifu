//! Reminders: "remind me in 20 minutes to stretch", "remind me at 5pm to call
//! mom". Simple phrasings are caught here before the model sees the message
//! (so even small models get them right); anything else can go through the
//! `set_reminder` tool. proactive.rs delivers them when due.

use chrono::{DateTime, Duration, Local, NaiveTime, TimeZone};
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub due: DateTime<Local>,
    pub text: String,
}

fn clean_text(t: &str) -> String {
    t.trim().trim_end_matches(['.', '!', '?']).trim().to_string()
}

/// Minutes from an amount and a unit like "min", "hours", "h"
fn minutes(n: i64, unit: &str) -> i64 {
    if unit.to_lowercase().starts_with('h') {
        n * 60
    } else {
        n
    }
}

/// Next time the clock shows h:m (today if still ahead, else tomorrow)
fn next_clock(now: DateTime<Local>, h: u32, m: u32) -> Option<DateTime<Local>> {
    let t = NaiveTime::from_hms_opt(h, m, 0)?;
    let today = Local.from_local_datetime(&now.date_naive().and_time(t)).earliest()?;
    Some(if today > now { today } else { today + Duration::days(1) })
}

fn clock_from(h: u32, m: u32, ampm: Option<&str>) -> Option<(u32, u32)> {
    let h = match ampm.map(str::to_lowercase).as_deref() {
        Some("pm") if h < 12 => h + 12,
        Some("am") if h == 12 => 0,
        _ => h,
    };
    (h < 24 && m < 60).then_some((h, m))
}

/// "remind me in 20 min to X", "remind me in an hour to X",
/// "remind me at 5pm to X", "remind me at 17:30 to X"
pub fn parse_request(text: &str, now: DateTime<Local>) -> Option<Parsed> {
    static RE: OnceLock<(Regex, Regex, Regex)> = OnceLock::new();
    let (after, half, at) = RE.get_or_init(|| {
        let tail = r"\s+(?:to\s+|that\s+|about\s+)?(.+)$";
        (
            Regex::new(&format!(
                r"(?is)\bremind me\s+in\s+(\d+|an?|one)\s*(m|mins?|minutes?|h|hrs?|hours?)\b{tail}"
            ))
            .unwrap(),
            Regex::new(&format!(r"(?is)\bremind me\s+in\s+half\s+an?\s+hour\b{tail}")).unwrap(),
            Regex::new(&format!(
                r"(?is)\bremind me\s+at\s+(\d{{1,2}})(?:[:.](\d{{2}}))?\s*(am|pm)?\b{tail}"
            ))
            .unwrap(),
        )
    });

    if let Some(c) = half.captures(text) {
        return Some(Parsed {
            due: now + Duration::minutes(30),
            text: clean_text(&c[1]),
        });
    }
    if let Some(c) = after.captures(text) {
        let n: i64 = c[1].parse().unwrap_or(1); // "an hour", "a minute", "one hour"
        let mins = minutes(n, &c[2]);
        if !(1..=7 * 24 * 60).contains(&mins) {
            return None;
        }
        return Some(Parsed {
            due: now + Duration::minutes(mins),
            text: clean_text(&c[3]),
        });
    }
    if let Some(c) = at.captures(text) {
        let h: u32 = c[1].parse().ok()?;
        let m: u32 = c.get(2).map_or(Some(0), |m| m.as_str().parse().ok())?;
        let (h, m) = clock_from(h, m, c.get(3).map(|a| a.as_str()))?;
        return Some(Parsed {
            due: next_clock(now, h, m)?,
            text: clean_text(&c[4]),
        });
    }
    None
}

/// For the `set_reminder` tool: `in_minutes` or `at` ("17:30" / "5pm"), plus `text`.
pub fn from_tool(args: &serde_json::Value, now: DateTime<Local>) -> Result<Parsed, String> {
    let text = args
        .get("text")
        .or_else(|| args.get("what"))
        .or_else(|| args.get("message"))
        .and_then(|t| t.as_str())
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or("No reminder text given")?;
    let in_minutes = args.get("in_minutes").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_f64().map(|f| f.round() as i64))
            .or_else(|| v.as_str()?.trim().parse().ok())
    });
    if let Some(mins) = in_minutes {
        if !(1..=7 * 24 * 60).contains(&mins) {
            return Err("Reminders can be 1 minute to 7 days away".into());
        }
        return Ok(Parsed {
            due: now + Duration::minutes(mins),
            text,
        });
    }
    let at = args.get("at").and_then(|v| v.as_str()).ok_or("Say when: in_minutes or at")?;
    static AT: OnceLock<Regex> = OnceLock::new();
    let re = AT.get_or_init(|| Regex::new(r"(?i)^\s*(\d{1,2})(?:[:.](\d{2}))?\s*(am|pm)?\s*$").unwrap());
    let c = re.captures(at).ok_or_else(|| format!("Couldn't read the time \"{at}\""))?;
    let h: u32 = c[1].parse().map_err(|_| "Bad hour")?;
    let m: u32 = c.get(2).map_or(Ok(0), |m| m.as_str().parse()).map_err(|_| "Bad minute")?;
    let (h, m) = clock_from(h, m, c.get(3).map(|a| a.as_str())).ok_or("Bad time")?;
    Ok(Parsed {
        due: next_clock(now, h, m).ok_or("Bad time")?,
        text,
    })
}

/// "17:30", or "tomorrow 09:00"
pub fn describe_due(due: DateTime<Local>, now: DateTime<Local>) -> String {
    let time = due.format("%H:%M").to_string();
    match (due.date_naive() - now.date_naive()).num_days() {
        0 => time,
        1 => format!("tomorrow {time}"),
        _ => due.format("%a %-d %b %H:%M").to_string(),
    }
}

pub fn tool_definition() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "set_reminder",
            "description": "Set a reminder the companion will say out loud later. Use when the user asks \
                to be reminded of something. Give either in_minutes or at.",
            "parameters": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "What to remind them about" },
                    "in_minutes": { "type": "integer", "description": "Minutes from now" },
                    "at": { "type": "string", "description": "Clock time like 17:30 or 5pm" }
                },
                "required": ["text"]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 10, 14, 0, 0).unwrap()
    }

    #[test]
    fn relative_times() {
        let p = parse_request("Remind me in 20 min to stretch.", now()).unwrap();
        assert_eq!(p.due, now() + Duration::minutes(20));
        assert_eq!(p.text, "stretch");
        let p = parse_request("can you remind me in an hour to call mom", now()).unwrap();
        assert_eq!(p.due, now() + Duration::hours(1));
        assert_eq!(p.text, "call mom");
        let p = parse_request("remind me in half an hour about the oven", now()).unwrap();
        assert_eq!((p.due, p.text.as_str()), (now() + Duration::minutes(30), "the oven"));
        assert_eq!(parse_request("remind me in 2 hours that the build is done", now()).unwrap().due, now() + Duration::hours(2));
    }

    #[test]
    fn clock_times() {
        let p = parse_request("remind me at 5pm to submit", now()).unwrap();
        assert_eq!(p.due, Local.with_ymd_and_hms(2026, 11, 10, 17, 0, 0).unwrap());
        // 9:30 has passed today, so tomorrow
        let p = parse_request("remind me at 9:30 to check mail", now()).unwrap();
        assert_eq!(p.due, Local.with_ymd_and_hms(2026, 11, 11, 9, 30, 0).unwrap());
        assert_eq!(describe_due(p.due, now()), "tomorrow 09:30");
        assert!(parse_request("remind me at 25:00 to x", now()).is_none());
    }

    #[test]
    fn not_reminders() {
        assert!(parse_request("that reminds me of something", now()).is_none());
        assert!(parse_request("remind me what we said", now()).is_none());
    }

    #[test]
    fn tool_arguments() {
        let p = from_tool(&serde_json::json!({"text": "drink water", "in_minutes": 15}), now()).unwrap();
        assert_eq!(p.due, now() + Duration::minutes(15));
        let p = from_tool(&serde_json::json!({"text": "meeting", "at": "4:30 pm"}), now()).unwrap();
        assert_eq!(p.due, Local.with_ymd_and_hms(2026, 11, 10, 16, 30, 0).unwrap());
        assert!(from_tool(&serde_json::json!({"text": "x"}), now()).is_err());
        assert!(from_tool(&serde_json::json!({"in_minutes": 5}), now()).is_err());
    }
}
