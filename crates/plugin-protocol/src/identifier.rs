//! Naming the thing a tool creates for a task: agents, worktrees, directories
//! ([ADR-0071](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0071-task-identifier-naming.md)).
//!
//! Every tool totsuka drives wants a name for what it creates — herdr names
//! the agent, orca names the worktree, the Orchestrator names the worktree
//! directory — and each imposes its own alphabet, its own length, and its own
//! rule about the first character. The **constraints differ; the procedure
//! does not**, so the constraints are declared per tool by implementing
//! [`IdentifierPolicy`] and the procedure lives here, once.
//!
//! # Why this is not three sanitizers
//!
//! It was three, and the arithmetic was wrong in one of them (#645). herdr's
//! budget spent the separator **after** checking the remaining length, so an
//! id whose alphanumeric run happened to end on the 20th character produced a
//! 33-character name; `agent.start` answered `invalid_agent_name`, and the
//! dispatch failed, auto-retried three times, and failed for good. The other
//! two implementations were not wrong, which is the point — the bug was
//! invisible precisely because nothing compared them.
//!
//! The property test in this module runs against any [`IdentifierPolicy`], so
//! a tool added later inherits the check instead of restating it.
//!
//! # Shape
//!
//! ```text
//! <prefix><task number><sep><8 hex of sha256(source ∥ source id)>
//!    t-         3        -              9f3c2a1e
//! ```
//!
//! - **The task number is the readable half** — [`tasks.id`, the number that
//!   appears in the logs (`task_id=3`), in `totsuka status`, and in
//!   `totsuka task retry 3`](IdentifierCore::task_number). A name that shares
//!   it can be carried straight back to the task; the source's own id, which
//!   this used to carry, could not (a truncated Slack timestamp or a slice of
//!   a base64 GitHub node id identifies nothing to a human).
//! - **The hash is what makes the name unique**, and it is kept even when
//!   nothing is truncated: the task number is unique within *one* `state.db`,
//!   so two totsuka instances driving one herdr would otherwise collide on
//!   `t-3`. Hashing `source ∥ source_task_id` rather than the id alone keeps
//!   two sources that both call something `42` apart.
//! - **Changing any of this renames every task's agent, once.** A name is
//!   derived, never stored, so the build that computes it is the only
//!   authority — and during an upgrade a live agent started by the previous
//!   build answers to the previous name. For the one task that is retried
//!   across that window, `agent_name_taken` cannot recognise its own orphan
//!   (ADR-0032 D-3) and a second agent is started beside it. The orphan stays
//!   detectable, because `totsuka doctor` finds it through the workspace
//!   label — which carries the source's task id and is unaffected by any of
//!   this (ADR-0013). Keeping the old digest for the fallback would not narrow
//!   the window: the readable half changes too, and only for the *fallback*,
//!   so the main path would rename regardless.
//! - **The session row is deliberately absent.** The name of a task's agent
//!   has to be the same on every dispatch of that task, because
//!   [ADR-0032](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0032-herdr-protocol-17.md)
//!   D-3 reads `agent_name_taken` as "an agent for this task is still alive,
//!   so something was not cleaned up" and refuses rather than renaming. A
//!   per-dispatch name would make that refusal unreachable and let orphans
//!   accumulate silently. It is also unavailable: the worktree is named before
//!   the session row is reserved.

use sha2::{Digest, Sha256};

use crate::methods::TaskDispatchParams;

/// Hex characters of the digest an identifier carries.
///
/// Four bytes. Enough that a collision inside one operator's set of live
/// agents is not a thing that happens, short enough to leave most of herdr's
/// 32 for the readable half.
pub const HASH_CHARS: usize = 8;

/// Whether a tool's alphabet includes upper case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// Fold to lower case (herdr's `[a-z][a-z0-9_-]{0,31}`).
    Lower,
    /// Keep the case as given.
    Preserve,
}

/// What an identifier is built from.
#[derive(Debug, Clone, Copy)]
pub struct IdentifierCore<'a> {
    /// The Orchestrator's own task number (`tasks.id`), when it sent one.
    ///
    /// `None` from an Orchestrator predating protocol 0.7.1, which is the
    /// only case where the source's id is used for the readable half. It is a
    /// degradation, not a failure: the name is still legal and still unique.
    pub task_number: Option<i64>,
    /// The plugin instance the task came from, as the Orchestrator named it.
    pub source: &'a str,
    /// The source's own id for the task (a Slack `channel:ts`, a GitHub node
    /// id, a Notion page id). Hashed in full, whatever the readable half ends
    /// up being.
    pub source_task_id: &'a str,
}

impl<'a> IdentifierCore<'a> {
    /// The core carried by a `task/dispatch`.
    pub fn from_dispatch(params: &'a TaskDispatchParams) -> Self {
        Self {
            task_number: params.task_number,
            source: &params.task.source,
            source_task_id: &params.task.id,
        }
    }

    /// The [`HASH_CHARS`]-character digest half of this task's identifiers.
    ///
    /// Public because a caller may need it on its own — the worktree
    /// *location* template offers it as `{hash}` — and computing the same
    /// digest a second time elsewhere is exactly what this module exists to
    /// prevent.
    pub fn hash(&self) -> String {
        hash8(self.source, self.source_task_id)
    }

    /// The readable half: the task number when there is one, else the source's
    /// id, sanitized for `case` and `separator`.
    ///
    /// Public for the same reason as [`hash`](Self::hash) — a caller that
    /// wants only this half must not re-derive it.
    pub fn readable(&self, case: Case, separator: Option<char>) -> String {
        match self.task_number {
            Some(n) => sanitize(&n.to_string(), case, separator),
            None => sanitize(self.source_task_id, case, separator),
        }
    }
}

/// The constraints one tool puts on the identifiers it accepts.
///
/// Implement the four constraint methods; [`identifier`](Self::identifier)
/// then produces a name that satisfies them. Overriding it is possible and
/// unwise — it is the shared procedure that the property test covers, and the
/// reason this trait exists at all.
///
/// # Two invariants an implementation owes
///
/// - **`max_len`, when present, must leave room**: at least
///   `prefix().len() + HASH_CHARS`. A tool whose limit is smaller than its own
///   prefix plus the hash cannot be served, and the procedure will exceed the
///   limit rather than truncate the hash (a short hash is a wrong name, which
///   is worse than a rejected one).
/// - **A tool that requires a letter first needs a `prefix` that starts with
///   one.** Nothing else can guarantee it: the task number is a digit, and so
///   is the hash half the time.
pub trait IdentifierPolicy {
    /// Fixed leading string (`"t-"`, `"totsuka-"`, or `""`).
    fn prefix(&self) -> &str;

    /// Total length limit **in bytes**, or `None` when the tool has none.
    ///
    /// Bytes rather than characters because that is what a truncation can
    /// honour without lying. A tool that states its limit in *characters* —
    /// herdr does — is served exactly as long as its alphabet is ASCII, which
    /// [`extra_allowed`](Self::extra_allowed) and [`case`](Self::case) already
    /// constrain it to be for every tool that has one.
    fn max_len(&self) -> Option<usize>;

    /// Whether upper case survives.
    fn case(&self) -> Case;

    /// The non-alphanumeric characters the tool accepts.
    ///
    /// The **first** is used as the separator (see [`separator`](Self::separator));
    /// the rest only widen what the property test accepts as legal output.
    fn extra_allowed(&self) -> &[char];

    /// The character that joins the parts, or `None` for a tool whose alphabet
    /// is alphanumerics only (the parts are then simply adjacent).
    fn separator(&self) -> Option<char> {
        self.extra_allowed().first().copied()
    }

    /// The identifier for `core` under these constraints.
    fn identifier(&self, core: &IdentifierCore<'_>) -> String {
        build(
            self.prefix(),
            self.max_len(),
            self.case(),
            self.separator(),
            core,
        )
    }
}

/// The shared procedure: readable half, then the budget, then the hash.
///
/// Free rather than inlined into the trait so that an implementation which
/// *does* override [`IdentifierPolicy::identifier`] can still reach it.
pub fn build(
    prefix: &str,
    max_len: Option<usize>,
    case: Case,
    separator: Option<char>,
    core: &IdentifierCore<'_>,
) -> String {
    let hash = core.hash();
    // `len_utf8`, not 1: a policy may declare a non-ASCII separator, and a
    // budget counted in characters against a limit counted in bytes overflows
    // it — the same shape as the bug this module replaced.
    let sep_len = separator.map_or(0, char::len_utf8);

    let readable = core.readable(case, separator);

    // The separator between the readable half and the hash is spent **here**,
    // before the truncation, not after it. Spending it afterwards is the
    // off-by-one this module exists to make unrepeatable (#645).
    let readable = match max_len {
        Some(max) => {
            let reserved = prefix.len() + sep_len + HASH_CHARS;
            truncate(&readable, max.saturating_sub(reserved), separator)
        }
        None => readable,
    };

    let mut out = String::with_capacity(prefix.len() + readable.len() + sep_len + HASH_CHARS);
    out.push_str(prefix);
    if !readable.is_empty() {
        out.push_str(&readable);
        if let Some(sep) = separator {
            out.push(sep);
        }
    }
    out.push_str(&hash);
    out
}

/// The first [`HASH_CHARS`] hex characters of `sha256(source ∥ 0x00 ∥ id)`.
///
/// The `0x00` keeps `("ab", "c")` and `("a", "bc")` apart, so a source name
/// ending where an id begins cannot forge the identity of another task.
fn hash8(source: &str, source_task_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(source.as_bytes());
    hash.update([0u8]);
    hash.update(source_task_id.as_bytes());
    let digest = hash.finalize();
    digest
        .iter()
        .take(HASH_CHARS / 2)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// ASCII alphanumerics, case-mapped; every other run becomes one separator.
///
/// Leading and trailing separators are never emitted, so the result can be
/// concatenated with a prefix or a hash without producing a doubled or
/// dangling separator. The output is ASCII **apart from a separator the policy
/// chose** — which is why [`truncate`] walks to a character boundary rather
/// than indexing bytes.
fn sanitize(input: &str, case: Case, separator: Option<char>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_separator = false;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_separator
                && !out.is_empty()
                && let Some(sep) = separator
            {
                out.push(sep);
            }
            pending_separator = false;
            out.push(match case {
                Case::Lower => c.to_ascii_lowercase(),
                Case::Preserve => c,
            });
        } else {
            // Collapsed rather than emitted: `a::b` is one separator, and a
            // trailing run leaves nothing behind because it is never flushed.
            pending_separator = true;
        }
    }
    out
}

/// `value` cut to `budget` **bytes**, with any separator the cut exposed at the
/// end removed.
///
/// The cut walks to a character boundary rather than indexing: `value` is
/// [`sanitize`] output, which is ASCII apart from a separator the policy chose,
/// and `&value[..budget]` would panic if that separator straddled the budget.
fn truncate(value: &str, budget: usize, separator: Option<char>) -> String {
    if value.len() <= budget {
        return value.to_string();
    }
    let end = value
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|end| *end <= budget)
        .last()
        .unwrap_or(0);
    let cut = &value[..end];
    match separator {
        Some(sep) => cut.trim_end_matches(sep).to_string(),
        None => cut.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// herdr's `agent.start` name: `[a-z][a-z0-9_-]{0,31}`.
    struct Herdr;
    impl IdentifierPolicy for Herdr {
        fn prefix(&self) -> &str {
            "t-"
        }
        fn max_len(&self) -> Option<usize> {
            Some(32)
        }
        fn case(&self) -> Case {
            Case::Lower
        }
        fn extra_allowed(&self) -> &[char] {
            &['-', '_']
        }
    }

    /// orca's `worktree create --name`: no documented limit.
    struct Orca;
    impl IdentifierPolicy for Orca {
        fn prefix(&self) -> &str {
            "totsuka-"
        }
        fn max_len(&self) -> Option<usize> {
            None
        }
        fn case(&self) -> Case {
            Case::Preserve
        }
        fn extra_allowed(&self) -> &[char] {
            &['-', '_']
        }
    }

    /// The tightest shape the trait admits: no prefix, no separator, and a
    /// limit with nothing to spare beyond the hash.
    struct Bare;
    impl IdentifierPolicy for Bare {
        fn prefix(&self) -> &str {
            ""
        }
        fn max_len(&self) -> Option<usize> {
            Some(HASH_CHARS + 4)
        }
        fn case(&self) -> Case {
            Case::Lower
        }
        fn extra_allowed(&self) -> &[char] {
            &[]
        }
    }

    fn core<'a>(task_number: Option<i64>, source: &'a str, id: &'a str) -> IdentifierCore<'a> {
        IdentifierCore {
            task_number,
            source,
            source_task_id: id,
        }
    }

    /// Every constraint a policy declares, checked on one name.
    fn is_legal<P: IdentifierPolicy>(policy: &P, name: &str) -> Result<(), String> {
        if !name.starts_with(policy.prefix()) {
            return Err(format!("{name}: prefix missing"));
        }
        if let Some(max) = policy.max_len()
            && name.len() > max
        {
            return Err(format!("{name}: {} > {max}", name.len()));
        }
        for c in name.chars() {
            let allowed = c.is_ascii_digit()
                || c.is_ascii_lowercase()
                || (policy.case() == Case::Preserve && c.is_ascii_uppercase())
                || policy.extra_allowed().contains(&c);
            if !allowed {
                return Err(format!("{name}: illegal character {c:?}"));
            }
        }
        if name.is_empty() {
            return Err("empty".into());
        }
        Ok(())
    }

    /// Deterministic pseudo-random ids, so the property below covers shapes
    /// nobody thought to write down — including the one that produced the
    /// 33-character name (#645).
    fn nasty_ids() -> Vec<String> {
        // The alphabet is what real source ids are made of, plus the
        // separators and cases that the sanitizer has to fold.
        let alphabet: Vec<char> = "abzAZ09:._-/ 　あ".chars().collect();
        let mut ids = vec![
            String::new(),
            ":::".into(),
            "::9".into(),
            "42".into(),
            // The two live shapes, and the 9-character-channel Slack id whose
            // alphanumeric run ends exactly on the 20th character.
            "C0BEYU0E95Y:1700000000.614309".into(),
            "C5UTJ195W:1700000000.369289".into(),
            "I_kwDOTrfAp88AAAABLKoO_Q".into(),
            "1f2a3b4c-5d6e-7f80-9a1b-2c3d4e5f6a7b".into(),
        ];
        // xorshift64*, so the corpus is the same on every machine and every run.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        for _ in 0..2_000 {
            let len = (next() % 48) as usize;
            ids.push(
                (0..len)
                    .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                    .collect(),
            );
        }
        ids
    }

    /// The property the three hand-written sanitizers were each asserting
    /// about themselves, now asserted once about anything implementing the
    /// trait: whatever comes in, the name obeys the policy.
    #[test]
    fn any_input_produces_a_name_the_policy_accepts() {
        let numbers = [None, Some(1), Some(3), Some(999_999), Some(i64::MAX)];
        for id in nasty_ids() {
            for number in numbers {
                for source in ["slack", "github", "", "a-very-long-instance-name"] {
                    let c = core(number, source, &id);
                    is_legal(&Herdr, &Herdr.identifier(&c)).unwrap();
                    is_legal(&Orca, &Orca.identifier(&c)).unwrap();
                    is_legal(&Bare, &Bare.identifier(&c)).unwrap();
                }
            }
        }
    }

    /// The shape the whole design is for: the name carries the number that
    /// `totsuka status` and `totsuka task retry` use.
    #[test]
    fn the_task_number_is_the_readable_half() {
        let name = Herdr.identifier(&core(Some(3), "slack", "C0BEYU0E95Y:1700000000.614309"));
        assert!(name.starts_with("t-3-"), "{name}");
        assert_eq!(name.len(), 2 + 1 + 1 + HASH_CHARS);
        // The same core, spelled for another tool: prefixes differ, the rest
        // is byte-identical, so one grep finds both.
        let orca = Orca.identifier(&core(Some(3), "slack", "C0BEYU0E95Y:1700000000.614309"));
        assert_eq!(
            orca.strip_prefix("totsuka-"),
            name.strip_prefix("t-"),
            "{orca} vs {name}"
        );
    }

    /// Truncation is what makes the readable half unsafe on its own, so the
    /// hash has to separate ids that share a prefix — the case that would
    /// otherwise point two tasks at one agent.
    #[test]
    fn the_hash_separates_ids_that_share_a_readable_prefix() {
        let a = Herdr.identifier(&core(None, "slack", "C0BEYU0E95Y:1700000000.111111"));
        let b = Herdr.identifier(&core(None, "slack", "C0BEYU0E95Y:1700000000.222222"));
        assert_ne!(a, b);
        // …and stable, because a re-dispatch of the same task has to compute
        // the same name (ADR-0032 D-3).
        assert_eq!(
            a,
            Herdr.identifier(&core(None, "slack", "C0BEYU0E95Y:1700000000.111111"))
        );
    }

    /// The task number is unique per `state.db`, so two instances driving one
    /// tool must not agree on a name for their respective task 3.
    #[test]
    fn the_hash_separates_two_instances_sharing_a_task_number() {
        let a = Herdr.identifier(&core(Some(3), "slack", "C0BEYU0E95Y:1700000000.111111"));
        let b = Herdr.identifier(&core(Some(3), "slack", "C0BEYU0E95Y:1700000000.222222"));
        assert_ne!(a, b);
        // And the same id from two source instances is two tasks.
        let c = Herdr.identifier(&core(Some(3), "github", "42"));
        let d = Herdr.identifier(&core(Some(3), "notion", "42"));
        assert_ne!(c, d);
    }

    /// The `0x00` in the hash input: a source name ending where an id begins
    /// must not forge another task's identity.
    #[test]
    fn the_hash_input_is_unambiguously_split() {
        assert_ne!(
            Herdr.identifier(&core(None, "ab", "c")),
            Herdr.identifier(&core(None, "a", "bc"))
        );
    }

    /// Without a task number the source id is the readable half — the same
    /// **readable prefix** the plugin produced before 0.7.1, minus the
    /// overflow. Not the same *name*: the digest half now covers the source as
    /// well as the id (see the module docs on renaming).
    #[test]
    fn the_fallback_keeps_a_readable_prefix() {
        let name = Herdr.identifier(&core(None, "slack", "C0BEYU0E95Y:1700000000.614309"));
        assert!(name.starts_with("t-c0beyu0e95y-"), "{name}");
    }

    /// The exact shape that shipped a 33-character name: the alphanumeric run
    /// ends on the 20th character, so the separator spent *after* the length
    /// check pushed the total one over herdr's 32 (#645).
    ///
    /// 31, not 32, and deliberately so — the cut lands on the separator, which
    /// is then dropped rather than left dangling. Spending the budget early is
    /// what makes the overflow unrepresentable; coming in under it sometimes
    /// is the price, and it costs a character of an id nobody reads.
    #[test]
    fn the_separator_is_spent_inside_the_budget() {
        let name = Herdr.identifier(&core(None, "slack", "C5UTJ195W:1700000000.369289"));
        assert_eq!(name.len(), 31, "{name}");
        assert!(!name.ends_with('-'), "{name}");
        is_legal(&Herdr, &name).unwrap();
    }

    /// An id with no alphanumerics at all leaves no readable half; the name is
    /// then the prefix and the hash, with no dangling separator between them.
    #[test]
    fn an_unreadable_id_still_produces_a_legal_name() {
        let name = Herdr.identifier(&core(None, "slack", ":::"));
        assert_eq!(name, format!("t-{}", &name[2..]));
        assert!(!name.starts_with("t--"), "{name}");
        is_legal(&Herdr, &name).unwrap();
    }

    /// A policy may declare a multi-byte separator, and the budget is in
    /// bytes: the cut must not overflow the limit nor split the character.
    #[test]
    fn a_multi_byte_separator_neither_overflows_nor_splits() {
        struct Wide;
        impl IdentifierPolicy for Wide {
            fn prefix(&self) -> &str {
                "w"
            }
            fn max_len(&self) -> Option<usize> {
                Some(24)
            }
            fn case(&self) -> Case {
                Case::Lower
            }
            fn extra_allowed(&self) -> &[char] {
                // 3 bytes each.
                &['…', '—']
            }
        }
        for id in nasty_ids() {
            let name = Wide.identifier(&core(None, "slack", &id));
            assert!(name.len() <= 24, "{name} is {} bytes", name.len());
            // Still a string: a split character would have panicked above, and
            // a truncated one would leave a replacement here.
            assert!(!name.contains('\u{FFFD}'), "{name}");
            is_legal(&Wide, &name).unwrap();
        }
    }

    /// `Preserve` is a tool's choice, not a suggestion.
    #[test]
    fn case_is_folded_only_where_the_tool_asks() {
        let c = core(None, "github", "I_kwDOTrfAp88");
        assert!(Herdr.identifier(&c).starts_with("t-i-kwdotrfap88"));
        assert!(Orca.identifier(&c).starts_with("totsuka-I-kwDOTrfAp88"));
    }
}
