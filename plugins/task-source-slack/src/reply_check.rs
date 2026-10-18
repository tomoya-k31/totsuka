//! The reply check (ADR-0120): before a mention becomes a task, ask the
//! `[llm]` backend whether the operator needs to write a reply to it at all.
//!
//! FYIs, announcements, a bare thank-you, "react with an emoji when done"
//! requests and a bot's asks need none, yet each one would otherwise cost a
//! worktree, an agent session and a draft the operator then rejects. The rule
//! lives in [`SlackPrompts::reply_check_criteria`] so an installation can name
//! its own conventions.
//!
//! The question rides [`llm_choice`] as a two-option choice, so a chat model
//! and a decisions model (TypeSafe Jev) both answer it. **An unsure answer
//! keeps the mention**: only `no_reply` at or above
//! [`SlackConfig::reply_check_threshold`] drops it.

use llm_choice::{
    Answer, ChatPrompt, ChoiceClassifier, ChoiceOption, ChoiceRequest, ClassifyError,
    HttpTransport, Question,
};
use plugin_sdk::template;

use crate::config::{LlmConfig, SlackConfig, SlackPrompts};

/// The question id: the decisions `questions` key, and the field a chat answer
/// names its choice in.
const QUESTION_ID: &str = "reply";
const REPLY: &str = "reply";
const NO_REPLY: &str = "no_reply";

/// What the check decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Go on: resolve the repository and submit the task.
    Reply,
    /// Drop the mention: it needs no reply.
    NoReply {
        /// Why, for the operator's notification. A chat model's own sentence;
        /// a decisions model's probability ("返信不要 84%").
        reason: String,
    },
}

/// The mention as the check reads it.
pub struct Subject<'a> {
    /// The message text.
    pub text: &'a str,
    /// Whether a bot posted it.
    pub from_bot: bool,
    /// The thread's recent messages, `name: text` per line; empty when none.
    pub thread_context: &'a str,
}

/// Ask whether the mention needs a reply.
///
/// `Err` is a check that could not be made — transport, status, an unreadable
/// answer — and the caller stops rather than guessing (ADR-0120).
pub async fn check<T: HttpTransport>(
    transport: &T,
    llm: &LlmConfig,
    config: &SlackConfig,
    subject: &Subject<'_>,
) -> Result<Verdict, ClassifyError> {
    let classifier = crate::llm::classifier(transport, llm);
    let decisions = matches!(llm.backend, crate::config::LlmBackend::Decisions { .. });
    let answer = classifier
        .choose(&request(&config.prompts, subject))
        .await?;
    Ok(verdict(answer, config.reply_check_threshold, decisions))
}

/// The check as a [`ChoiceRequest`].
fn request(prompts: &SlackPrompts, subject: &Subject<'_>) -> ChoiceRequest {
    let sender = if subject.from_bot { "bot" } else { "human" };
    let mut fields = vec![
        ("Sender".to_string(), sender.to_string()),
        ("Mention".to_string(), subject.text.to_string()),
    ];
    if !subject.thread_context.is_empty() {
        fields.push((
            "Thread context".to_string(),
            subject.thread_context.to_string(),
        ));
    }
    ChoiceRequest {
        subject: fields,
        question: Question {
            id: QUESTION_ID.to_string(),
            instructions: prompts.reply_check_criteria.clone(),
            options: vec![
                ChoiceOption {
                    key: REPLY.to_string(),
                    description: Some("The operator should write a reply.".to_string()),
                },
                ChoiceOption {
                    key: NO_REPLY.to_string(),
                    description: Some("No written reply is needed.".to_string()),
                },
            ],
            none: None,
        },
        chat_prompt: ChatPrompt {
            system: template::render(
                &prompts.reply_check_system,
                &[("criteria", prompts.reply_check_criteria.as_str())],
            ),
            // Single-pass: the message and the thread are Slack content the
            // author chose, so a literal `{sender}` in them stays literal.
            user: template::render(
                &prompts.reply_check_user,
                &[
                    ("sender", sender),
                    ("mention_text", subject.text),
                    ("thread_context", subject.thread_context),
                ],
            ),
            correction: Some(prompts.reply_check_correction.clone()),
        },
    }
}

/// Only a confident `no_reply` drops the mention.
fn verdict(answer: Answer, threshold: f64, decisions: bool) -> Verdict {
    match answer {
        Answer::Chosen {
            key,
            confidence,
            reason,
        } if key == NO_REPLY && confidence >= threshold => Verdict::NoReply {
            // A decisions model writes no prose; its "reason" is the
            // distribution, which says nothing a percentage does not.
            reason: if decisions || reason.is_empty() {
                format!("返信不要 {:.0}%", confidence * 100.0)
            } else {
                reason
            },
        },
        _ => Verdict::Reply,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chosen(key: &str, confidence: f64, reason: &str) -> Answer {
        Answer::Chosen {
            key: key.into(),
            confidence,
            reason: reason.into(),
        }
    }

    #[test]
    fn only_a_confident_no_reply_drops_the_mention() {
        assert_eq!(
            verdict(chosen(NO_REPLY, 0.9, "FYI です"), 0.7, false),
            Verdict::NoReply {
                reason: "FYI です".into()
            }
        );
        // At the threshold counts; below it keeps the mention.
        assert!(matches!(
            verdict(chosen(NO_REPLY, 0.7, "r"), 0.7, false),
            Verdict::NoReply { .. }
        ));
        assert_eq!(
            verdict(chosen(NO_REPLY, 0.69, "r"), 0.7, false),
            Verdict::Reply
        );
        assert_eq!(
            verdict(chosen(REPLY, 0.99, "r"), 0.7, false),
            Verdict::Reply
        );
    }

    #[test]
    fn a_decisions_reason_is_the_probability() {
        assert_eq!(
            verdict(
                chosen(NO_REPLY, 0.84, "jev: no_reply p=0.84 (next: reply 0.16)"),
                0.7,
                true
            ),
            Verdict::NoReply {
                reason: "返信不要 84%".into()
            }
        );
    }

    #[test]
    fn the_request_names_the_sender_and_keeps_slack_text_literal() {
        let prompts = SlackPrompts::default();
        let request = request(
            &prompts,
            &Subject {
                text: "共有です {sender}",
                from_bot: true,
                thread_context: "",
            },
        );
        assert_eq!(
            request.subject,
            vec![
                ("Sender".to_string(), "bot".to_string()),
                ("Mention".to_string(), "共有です {sender}".to_string()),
            ],
            "no thread context → no empty field"
        );
        assert!(request.chat_prompt.user.contains("## Sender\nbot"));
        assert!(
            request.chat_prompt.user.contains("共有です {sender}"),
            "single-pass: a placeholder in the message is not expanded"
        );
        assert!(
            request
                .chat_prompt
                .system
                .contains(&prompts.reply_check_criteria),
            "the rule is spliced into the chat system prompt"
        );
        assert_eq!(request.question.instructions, prompts.reply_check_criteria);
        assert!(request.question.none.is_none());
    }
}
