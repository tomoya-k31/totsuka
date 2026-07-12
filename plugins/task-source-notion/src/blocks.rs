//! Notion block ↔ Markdown conversion (F-03 / F-07).
//!
//! Reading ([`blocks_to_markdown`]) turns a page's block children into Markdown
//! so a task body can carry the page content. Writing ([`markdown_to_blocks`])
//! is the inverse for `result/publish`, splitting text to Notion's 2000-char
//! per-rich-text limit. `v1` covers the major block types; anything else is
//! rendered as its plain text (never dropped silently).

use serde_json::{Value, json};

/// Notion's maximum characters per rich-text string. Longer content is split
/// across multiple blocks of the same kind on publish.
pub const MAX_RICH_TEXT_LEN: usize = 2000;

/// Render a page's block children as Markdown. Unknown block types fall back to
/// their rich-text as a plain paragraph rather than being dropped.
pub fn blocks_to_markdown(blocks: &[Value]) -> String {
    let mut lines = Vec::new();
    for block in blocks {
        let kind = block["type"].as_str().unwrap_or("");
        let inner = &block[kind];
        let text = rich_text_plain(&inner["rich_text"]);
        let line = match kind {
            "heading_1" => format!("# {text}"),
            "heading_2" => format!("## {text}"),
            "heading_3" => format!("### {text}"),
            "bulleted_list_item" => format!("- {text}"),
            "numbered_list_item" => format!("1. {text}"),
            "to_do" => {
                let mark = if inner["checked"].as_bool() == Some(true) {
                    "x"
                } else {
                    " "
                };
                format!("- [{mark}] {text}")
            }
            "quote" => format!("> {text}"),
            "code" => {
                let lang = inner["language"].as_str().unwrap_or("");
                format!("```{lang}\n{text}\n```")
            }
            // paragraph and any unsupported type: emit the plain text.
            _ => text,
        };
        lines.push(line);
    }
    lines.join("\n")
}

/// Join a Notion `rich_text` array into a plain string. Prefers the
/// API-provided `plain_text`, falling back to `text.content`. Shared with
/// [`client`](crate::client) for property (title/body/repo_hint) extraction.
pub fn rich_text_plain(rich_text: &Value) -> String {
    rich_text
        .as_array()
        .into_iter()
        .flatten()
        .map(|rt| {
            rt["plain_text"]
                .as_str()
                .or_else(|| rt["text"]["content"].as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// Convert Markdown `content` into Notion blocks for `result/publish` (F-07).
///
/// Line-oriented `v1`: headings (`#`/`##`/`###`), bullets (`-`/`*`), and quotes
/// (`>`) map to their block types; everything else is a paragraph. Blank lines
/// are dropped. Any line longer than [`MAX_RICH_TEXT_LEN`] is split into several
/// blocks of the same kind so no single rich-text string exceeds the limit.
pub fn markdown_to_blocks(content: &str) -> Vec<Value> {
    let mut blocks = Vec::new();
    for line in content.lines() {
        let (kind, text) = classify_line(line);
        if text.is_empty() {
            continue; // blank line (or a bare marker): nothing to add
        }
        for chunk in chunk_chars(text, MAX_RICH_TEXT_LEN) {
            blocks.push(text_block(kind, &chunk));
        }
    }
    blocks
}

/// Classify one Markdown line into a Notion block kind + its text payload.
fn classify_line(line: &str) -> (&'static str, &str) {
    let trimmed = line.trim_end();
    if let Some(rest) = trimmed.strip_prefix("### ") {
        ("heading_3", rest)
    } else if let Some(rest) = trimmed.strip_prefix("## ") {
        ("heading_2", rest)
    } else if let Some(rest) = trimmed.strip_prefix("# ") {
        ("heading_1", rest)
    } else if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        ("bulleted_list_item", rest)
    } else if let Some(rest) = trimmed.strip_prefix("> ") {
        ("quote", rest)
    } else {
        ("paragraph", trimmed)
    }
}

/// Build a single-rich-text block of `kind` carrying `text`.
fn text_block(kind: &str, text: &str) -> Value {
    json!({
        "object": "block",
        "type": kind,
        kind: { "rich_text": [ { "type": "text", "text": { "content": text } } ] }
    })
}

/// Split `s` into pieces of at most `max` **characters** (not bytes), so multi-
/// byte text (Japanese) is never cut mid-codepoint or over the Notion limit.
fn chunk_chars(s: &str, max: usize) -> Vec<String> {
    if s.chars().count() <= max {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for ch in s.chars() {
        current.push(ch);
        count += 1;
        if count == max {
            out.push(std::mem::take(&mut current));
            count = 0;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_major_block_types() {
        let blocks = vec![
            json!({ "type": "heading_1", "heading_1": { "rich_text": [{ "plain_text": "Title" }] } }),
            json!({ "type": "paragraph", "paragraph": { "rich_text": [{ "plain_text": "hello " }, { "plain_text": "world" }] } }),
            json!({ "type": "bulleted_list_item", "bulleted_list_item": { "rich_text": [{ "plain_text": "point" }] } }),
            json!({ "type": "to_do", "to_do": { "checked": true, "rich_text": [{ "plain_text": "done" }] } }),
            json!({ "type": "code", "code": { "language": "rust", "rich_text": [{ "plain_text": "fn main() {}" }] } }),
        ];
        let md = blocks_to_markdown(&blocks);
        assert_eq!(
            md,
            "# Title\nhello world\n- point\n- [x] done\n```rust\nfn main() {}\n```"
        );
    }

    #[test]
    fn unknown_block_falls_back_to_plain_text() {
        let blocks = vec![json!({
            "type": "callout",
            "callout": { "rich_text": [{ "plain_text": "note" }] }
        })];
        assert_eq!(blocks_to_markdown(&blocks), "note");
    }

    #[test]
    fn writes_headings_bullets_and_paragraphs() {
        let blocks = markdown_to_blocks("# Design\n\n- one\nplain text");
        assert_eq!(blocks.len(), 3, "blank line dropped");
        assert_eq!(blocks[0]["type"], "heading_1");
        assert_eq!(
            blocks[0]["heading_1"]["rich_text"][0]["text"]["content"],
            "Design"
        );
        assert_eq!(blocks[1]["type"], "bulleted_list_item");
        assert_eq!(blocks[2]["type"], "paragraph");
        assert_eq!(
            blocks[2]["paragraph"]["rich_text"][0]["text"]["content"],
            "plain text"
        );
    }

    #[test]
    fn splits_long_line_across_blocks_on_char_boundaries() {
        // 2500 multi-byte chars → two blocks (2000 + 500), never a broken char.
        let long = "あ".repeat(2500);
        let blocks = markdown_to_blocks(&long);
        assert_eq!(blocks.len(), 2);
        let first: &str = blocks[0]["paragraph"]["rich_text"][0]["text"]["content"]
            .as_str()
            .unwrap();
        let second: &str = blocks[1]["paragraph"]["rich_text"][0]["text"]["content"]
            .as_str()
            .unwrap();
        assert_eq!(first.chars().count(), MAX_RICH_TEXT_LEN);
        assert_eq!(second.chars().count(), 500);
    }
}
