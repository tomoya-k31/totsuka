//! Delegated 回答の DM 永続コピー。スレッド内エフェメラルは非永続・
//! 通知なしのため、質問者の Bot DM に「質問抜粋 + permalink + 回答全文」
//! を送って永続・通知ありの控えとする。すべて best-effort。

use crate::error::QaError;
use crate::slack::SlackClient;

/// 質問抜粋の最大文字数(chars ベース — バイトではない)。
const QUESTION_EXCERPT_CHARS: usize = 60;

/// DM 本文を組み立てる純粋関数。permalink 取得失敗時は 🔗 行ごと省略。
pub fn build_dm_text(question: &str, permalink: Option<&str>, answer: &str) -> String {
    // 改行・連続空白を単一スペースに潰してから chars ベースで切る
    // (バイト境界 slice は日本語質問で panic する)。
    let flat = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt: String = flat.chars().take(QUESTION_EXCERPT_CHARS).collect();
    if flat.chars().count() > QUESTION_EXCERPT_CHARS {
        excerpt.push('…');
    }
    let mut out = format!("💬 *質問:* 「{excerpt}」\n");
    if let Some(link) = permalink {
        out.push_str(&format!("🔗 {link}\n"));
    }
    out.push('\n');
    out.push_str(answer);
    out
}

/// permalink(best-effort)→ open_dm → post_message。
/// open_dm / post_message の失敗は Err で返し、呼び出し側が warn で握る。
pub async fn send_dm_copy(
    slack: &dyn SlackClient,
    user: &str,
    channel: &str,
    thread_ts: &str,
    question: &str,
    answer: &str,
) -> Result<(), QaError> {
    // permalink は装飾 — 取れなくても DM 本体は送る。
    // リンク先は常にスレッド親(最初の質問)。継続ターンでは抜粋と
    // リンク先の質問がズレるが、これは設計上の割り切り — AnswerInput は
    // 当該ターンのメッセージ ts を持たず、スレッドを開けば文脈全体が見える。
    let permalink = match slack.permalink(channel, thread_ts).await {
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!(error=%e, thread_ts, "permalink fetch failed; sending DM without link");
            None
        }
    };
    let dm_channel = slack.open_dm(user).await?;
    let text = build_dm_text(question, permalink.as_deref(), answer);
    slack.post_message(&dm_channel, None, &text).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slack::MockSlackClient;

    #[test]
    fn dm_text_has_excerpt_link_and_answer() {
        let t = build_dm_text("where is auth?", Some("https://x/p1"), "OK");
        assert_eq!(t, "💬 *質問:* 「where is auth?」\n🔗 https://x/p1\n\nOK");
    }

    #[test]
    fn dm_text_omits_link_line_when_no_permalink() {
        let t = build_dm_text("q", None, "A");
        assert_eq!(t, "💬 *質問:* 「q」\n\nA");
    }

    #[test]
    fn excerpt_truncates_at_60_chars_multibyte_safe() {
        // 70 文字の日本語質問 — バイト境界で切ると panic するのでここで検出。
        let q: String = "あ".repeat(70);
        let t = build_dm_text(&q, None, "A");
        let expected_excerpt: String = "あ".repeat(60);
        assert!(t.contains(&format!("「{expected_excerpt}…」")), "got: {t}");
        assert!(!t.contains(&"あ".repeat(61)), "must not exceed 60 chars");
    }

    #[test]
    fn excerpt_collapses_newlines() {
        let t = build_dm_text("line1\nline2\n\nline3", None, "A");
        assert!(t.contains("「line1 line2 line3」"), "got: {t}");
    }

    #[tokio::test]
    async fn send_posts_to_dm_channel_with_permalink() {
        let slack = MockSlackClient::new();
        send_dm_copy(&slack, "U1", "C1", "111.222", "Q?", "A!")
            .await
            .unwrap();
        let posts = slack.posts();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, "D_U1"); // DM channel
        assert_eq!(posts[0].1, None); // DM はスレッドではない
        assert!(posts[0]
            .2
            .contains("https://mock.slack/archives/C1/p111222"));
        assert!(posts[0].2.ends_with("A!"));
    }

    #[tokio::test]
    async fn permalink_failure_still_sends_dm_without_link() {
        let slack = MockSlackClient::new();
        slack.set_fail_permalink(true);
        send_dm_copy(&slack, "U1", "C1", "111.222", "Q?", "A!")
            .await
            .unwrap();
        let posts = slack.posts();
        assert_eq!(posts.len(), 1);
        assert!(!posts[0].2.contains("🔗"), "got: {}", posts[0].2);
    }

    #[tokio::test]
    async fn open_dm_failure_returns_err_and_posts_nothing() {
        let slack = MockSlackClient::new();
        slack.set_fail_open_dm(true);
        let err = send_dm_copy(&slack, "U1", "C1", "111.222", "Q?", "A!")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing_scope"), "got: {err}");
        assert!(slack.posts().is_empty());
    }
}
