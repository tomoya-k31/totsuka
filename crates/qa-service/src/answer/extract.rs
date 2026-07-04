//! Pure functions for pulling the answer text out of a pane snapshot.
//! Strategy: sentinel-bounded extraction first; on tag absence, fall back to
//! the last N lines before sentinel; UTF-8-safe truncate to max_chars.

#[derive(Debug, Clone, PartialEq)]
pub enum AnswerExtraction {
    TagDelimited(String),
    FallbackTail(String),
    Empty,
}

#[derive(Debug, Clone)]
pub struct ExtractConfig<'a> {
    pub sentinel: &'a str,
    pub open_tag: &'a str,
    pub close_tag: &'a str,
    pub max_chars: usize,
    pub fallback_tail_lines: usize,
}

pub fn extract(snapshot: &str, cfg: &ExtractConfig) -> AnswerExtraction {
    if snapshot.is_empty() {
        return AnswerExtraction::Empty;
    }
    // A reused pane still shows previous turns' answers, so always work on
    // the LAST sentinel / LAST tag block — earlier ones are stale turns.
    let bounded = match snapshot.rfind(cfg.sentinel) {
        Some(idx) => &snapshot[..idx],
        None => snapshot,
    };
    if let Some(o) = bounded.rfind(cfg.open_tag) {
        let after = o + cfg.open_tag.len();
        if let Some(rel_c) = bounded[after..].find(cfg.close_tag) {
            let body = &bounded[after..after + rel_c];
            return AnswerExtraction::TagDelimited(truncate(body, cfg.max_chars));
        }
    }
    // Fallback: last N lines of bounded section.
    let lines: Vec<&str> = bounded.lines().collect();
    let n = cfg.fallback_tail_lines.min(lines.len());
    if n == 0 {
        return AnswerExtraction::Empty;
    }
    let tail = lines[lines.len() - n..].join("\n");
    if tail.trim().is_empty() {
        AnswerExtraction::Empty
    } else {
        AnswerExtraction::FallbackTail(truncate(&tail, cfg.max_chars))
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = String::with_capacity(max_chars * 4);
    for (i, c) in s.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(c);
    }
    out
}
