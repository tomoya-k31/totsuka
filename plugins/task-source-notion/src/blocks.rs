//! Notion block ↔ Markdown conversion (F-03 / F-07).
//!
//! Reading ([`blocks_to_markdown`]) turns a page's block children into Markdown
//! so a task body can carry the page content. `v1` covers the major block
//! types; anything else is rendered as its plain text (never dropped silently).
//!
//! There is no write direction: the deliverable is the agent's to write (#398),
//! so `markdown_to_blocks` and its helpers went with `result/publish`.

use serde_json::Value;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
