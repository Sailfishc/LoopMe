//! The control block the agent appends to its reply.
//!
//! The agent ends a reply with `<loopme>{...json...}</loopme>`. The block is
//! hidden from the user (also while streaming) and tells LoopMe whether to
//! notify, which items to create or update, what to remember, and whether to
//! ask the user a question.

use serde::Deserialize;

pub const OPEN: &str = "<loopme>";
pub const CLOSE: &str = "</loopme>";

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Control {
    pub notify: bool,
    pub items: Vec<ItemOp>,
    pub memory: Vec<MemoryOp>,
    pub decision: Option<QuestionOp>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ItemOp {
    /// `create`, `suggest`, `update`, `checked`.
    pub op: String,
    pub id: Option<String>,
    pub title: Option<String>,
    pub detail: Option<String>,
    pub done_when: Option<String>,
    pub source: Option<String>,
    pub status: Option<String>,
    /// RFC 3339 time.
    pub next_check_at: Option<String>,
    pub next_check_in_minutes: Option<i64>,
    /// For `checked`: whether anything changed since the last check.
    pub changed: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MemoryOp {
    /// `add` or `update`.
    pub op: String,
    pub id: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct QuestionOp {
    pub title: String,
    pub detail: String,
    pub item_id: Option<String>,
}

/// Splits a final reply into display text and the parsed control block.
pub fn split(text: &str) -> (String, Option<Control>) {
    let Some(start) = text.rfind(OPEN) else {
        return (text.trim_end().to_string(), None);
    };
    let display = text[..start].trim_end().to_string();
    let rest = &text[start + OPEN.len()..];
    let body = match rest.find(CLOSE) {
        Some(end) => &rest[..end],
        None => rest,
    };
    let body = body.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    let control = serde_json::from_str::<Control>(body)
        .map_err(|e| tracing::warn!("bad control block: {e}: {body}"))
        .ok();
    (display, control)
}

/// Text that is safe to show while streaming: hides a control block and any
/// trailing partial prefix of `<loopme>`.
pub fn streaming_display(text: &str) -> &str {
    if let Some(start) = text.find(OPEN) {
        return text[..start].trim_end();
    }
    for k in (1..OPEN.len()).rev() {
        if text.ends_with(&OPEN[..k]) {
            return &text[..text.len() - k];
        }
    }
    text
}

/// Resolves an item op's next check time to Unix ms.
pub fn next_check_ms(op: &ItemOp, now_ms: i64) -> Option<i64> {
    if let Some(m) = op.next_check_in_minutes {
        return Some(now_ms + m.max(1) * 60_000);
    }
    let s = op.next_check_at.as_deref()?;
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_extracts_block() {
        let t = "好的，我会盯着。\n\n<loopme>{\"notify\":true,\"items\":[{\"op\":\"create\",\"title\":\"PR #12\",\"nextCheckInMinutes\":60}]}</loopme>";
        let (d, c) = split(t);
        assert_eq!(d, "好的，我会盯着。");
        let c = c.unwrap();
        assert!(c.notify);
        assert_eq!(c.items[0].title.as_deref(), Some("PR #12"));
        assert_eq!(next_check_ms(&c.items[0], 0), Some(3_600_000));
    }

    #[test]
    fn split_without_block() {
        assert_eq!(split("hello \n"), ("hello".into(), None));
    }

    #[test]
    fn split_with_fenced_json() {
        let (_, c) = split("x <loopme>\n```json\n{\"notify\":false}\n```\n</loopme>");
        assert_eq!(c, Some(Control::default()));
    }

    #[test]
    fn streaming_hides_partial_tag() {
        assert_eq!(streaming_display("abc <loo"), "abc ");
        assert_eq!(streaming_display("abc <loopme>{\"no"), "abc");
        assert_eq!(streaming_display("a < b"), "a < b");
    }
}
