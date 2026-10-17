//! One routine = one `routines/<host>/<name>.md` file (#918).
//!
//! ```markdown
//! ---
//! repo: my-repo              # required: a `[[repositories]].name`
//! schedule: "0 9 * * 1-5"    # required: five-field cron, local time
//! workflow: nightly          # optional when one workflow names this source
//! catchup_window: 6h         # optional: overrides `[routine].catchup_window`
//! enabled: false             # optional, default true: stops the schedule
//! ---
//! The prompt.
//! ```
//!
//! The frontmatter is flat `key: value` lines, not YAML: a value may be
//! wrapped in `"…"` or `'…'`, a `#` after a space starts a comment, and an
//! unknown or repeated key is an error rather than something to ignore.

use std::path::Path;

use time::Duration;

use crate::cron::Schedule;

/// A parsed routine.
#[derive(Debug, Clone)]
pub struct Routine {
    /// The file name without `.md` — the routine's identity.
    pub name: String,
    /// The `[[repositories]].name` it runs in.
    pub repo: String,
    /// When it runs.
    pub schedule: Schedule,
    /// `schedule` as written, for messages.
    pub schedule_text: String,
    /// The workflow to submit under, when the routine names one.
    pub workflow: Option<String>,
    /// How far back a missed slot is still run, when the routine overrides
    /// the plugin's default.
    pub catchup_window: Option<Duration>,
    /// Whether the schedule fires. A disabled routine can still be run by
    /// hand.
    pub enabled: bool,
    /// When its file was last written, as read by [`load_dir`]. A slot from
    /// before this is not caught up: a routine created, re-enabled or edited
    /// just now does not owe the runs from before it said so.
    pub written: Option<time::OffsetDateTime>,
    /// The body: what the agent is asked to do.
    pub prompt: String,
}

impl Routine {
    /// Whether this routine can run against the configured `repos`
    /// (`[[repositories]]` names) and `workflows` (those drawing from the
    /// routine source), and if so the workflow it runs under: the one it
    /// names, or the only one there is.
    pub fn check(&self, repos: &[String], workflows: &[String]) -> Result<String, String> {
        if !repos.contains(&self.repo) {
            return Err(format!(
                "repo `{}` is not a [[repositories]] name → one of: {}",
                self.repo,
                list(repos)
            ));
        }
        match (&self.workflow, workflows) {
            (Some(name), names) if names.contains(name) => Ok(name.clone()),
            (None, [only]) => Ok(only.clone()),
            (named, names) => {
                let what = match named {
                    Some(name) => {
                        format!("workflow `{name}` does not draw from the routine source")
                    }
                    None => "no `workflow` is named".to_string(),
                };
                Err(format!(
                    "{what} → name one of the workflows whose projects use the routine source: {}",
                    list(names)
                ))
            }
        }
    }
}

fn list(names: &[String]) -> String {
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

/// Parse a routine named `name` from the text of its file.
pub fn parse(name: &str, text: &str) -> Result<Routine, String> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or("the file does not start with a `---` frontmatter line")?;
    let (front, body) = match rest.split_once("\n---\n") {
        Some(parts) => parts,
        None => rest
            .strip_suffix("\n---")
            .map(|front| (front, ""))
            .ok_or("the frontmatter has no closing `---` line")?,
    };
    let (mut repo, mut schedule, mut workflow, mut window, mut enabled) =
        (None, None, None, None, None);
    for line in front.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("`{line}` is not a `key: value` line"))?;
        let (key, value) = (key.trim(), unquote(value.trim()));
        let slot = match key {
            "repo" => &mut repo,
            "schedule" => &mut schedule,
            "workflow" => &mut workflow,
            "catchup_window" => &mut window,
            "enabled" => &mut enabled,
            other => {
                return Err(format!(
                    "unknown key `{other}` → the keys are repo, schedule, workflow, catchup_window, enabled"
                ));
            }
        };
        if slot.replace(value.to_string()).is_some() {
            return Err(format!("`{key}` is written twice"));
        }
    }
    let repo = repo.ok_or("`repo` is missing → name a [[repositories]] entry")?;
    let schedule_text = schedule.ok_or("`schedule` is missing → write a five-field cron")?;
    let prompt = body.trim().to_string();
    if prompt.is_empty() {
        return Err("the prompt (the text after the frontmatter) is empty".into());
    }
    Ok(Routine {
        name: name.to_string(),
        repo,
        schedule: schedule_text.parse()?,
        schedule_text,
        workflow,
        catchup_window: window.as_deref().map(parse_duration).transpose()?,
        enabled: match enabled.as_deref() {
            None | Some("true") => true,
            Some("false") => false,
            Some(other) => return Err(format!("`enabled: {other}` → write true or false")),
        },
        written: None,
        prompt,
    })
}

/// `"0 9 * * *"  # weekdays` → `"0 9 * * *"`: a `#` after whitespace starts
/// a comment, outside quotes.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') if prev.is_whitespace() => return &line[..i],
            _ => {}
        }
        prev = c;
    }
    line
}

fn unquote(value: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = value.strip_prefix(q).and_then(|v| v.strip_suffix(q)) {
            return inner;
        }
    }
    value
}

/// `0`, `90m`, `24h`, `7d`.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let bad = || format!("`{text}` is not a duration → write e.g. 0, 90m, 24h or 7d");
    if text == "0" {
        return Ok(Duration::ZERO);
    }
    let split = text.len().checked_sub(1).ok_or_else(bad)?;
    let (n, unit) = text.split_at(split);
    // Unsigned and 32-bit: a negative window means nothing, and a huge one
    // would overflow `Duration` instead of failing here.
    let n: u32 = n.parse().map_err(|_| bad())?;
    match unit {
        "m" => Ok(Duration::minutes(n.into())),
        "h" => Ok(Duration::hours(n.into())),
        "d" => Ok(Duration::days(n.into())),
        _ => Err(bad()),
    }
}

/// Every `*.md` in `dir`, by name, each parsed or with the reason it could
/// not be. A missing directory is no routines, not an error.
pub fn load_dir(dir: &Path) -> Vec<(String, Result<Routine, String>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut routines: Vec<_> = entries
        .filter_map(|e| e.ok()?.path().into())
        .filter_map(|path: std::path::PathBuf| {
            let name = path.file_name()?.to_str()?.strip_suffix(".md")?.to_string();
            let written = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .map(time::OffsetDateTime::from);
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read: {e}"))
                .and_then(|text| parse(&name, &text))
                .map(|routine| Routine { written, ..routine });
            Some((name, parsed))
        })
        .collect();
    routines.sort_by(|a, b| a.0.cmp(&b.0));
    routines
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "---\n\
repo: totsuka\n\
schedule: \"0 9 * * 1-5\"   # weekdays\n\
workflow: nightly\n\
catchup_window: 6h\n\
enabled: false\n\
---\n\
Check the dependencies.\n\
\n\
Open a PR if any moved.\n";

    #[test]
    fn parses_every_key_and_keeps_the_body_as_the_prompt() {
        let r = parse("deps", FULL).unwrap();
        assert_eq!(r.name, "deps");
        assert_eq!(r.repo, "totsuka");
        assert_eq!(r.schedule_text, "0 9 * * 1-5");
        assert_eq!(r.workflow.as_deref(), Some("nightly"));
        assert_eq!(r.catchup_window, Some(Duration::hours(6)));
        assert!(!r.enabled);
        assert_eq!(
            r.prompt,
            "Check the dependencies.\n\nOpen a PR if any moved."
        );
    }

    #[test]
    fn the_optional_keys_default() {
        let r = parse("x", "---\nrepo: r\nschedule: '* * * * *'\n---\ngo\n").unwrap();
        assert_eq!(r.workflow, None);
        assert_eq!(r.catchup_window, None);
        assert!(r.enabled);
    }

    #[test]
    fn mistakes_are_errors_not_defaults() {
        for (text, needle) in [
            ("repo: r\n", "does not start"),
            ("---\nrepo: r\nschedule: * * * * *\n", "no closing"),
            (
                "---\nrepo: r\nschedule: * * * * *\nrepos: x\n---\ngo",
                "unknown key `repos`",
            ),
            (
                "---\nrepo: r\nrepo: s\nschedule: * * * * *\n---\ngo",
                "twice",
            ),
            ("---\nschedule: * * * * *\n---\ngo", "`repo` is missing"),
            ("---\nrepo: r\n---\ngo", "`schedule` is missing"),
            ("---\nrepo: r\nschedule: * * * * *\n---\n\n", "prompt"),
            ("---\nrepo: r\nschedule: * * * *\n---\ngo", "5 (minute"),
            (
                "---\nrepo: r\nschedule: * * * * *\nenabled: no\n---\ngo",
                "true or false",
            ),
            (
                "---\nrepo: r\nschedule: * * * * *\ncatchup_window: 1w\n---\ngo",
                "duration",
            ),
        ] {
            let err = parse("x", text).unwrap_err();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("0"), Ok(Duration::ZERO));
        assert_eq!(parse_duration("90m"), Ok(Duration::minutes(90)));
        assert_eq!(parse_duration("7d"), Ok(Duration::days(7)));
        for bad in ["", "h", "24", "1.5h", "-", "-5h", "99999999999d"] {
            assert!(parse_duration(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn load_dir_reads_markdown_only_and_reports_each_file() {
        let dir = std::env::temp_dir().join(format!("routine-load-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.md"), FULL).unwrap();
        std::fs::write(dir.join("a.md"), "nope").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let loaded = load_dir(&dir);
        let names: Vec<_> = loaded.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(loaded[0].1.is_err());
        assert!(loaded[1].1.as_ref().unwrap().written.is_some());
        assert!(load_dir(&dir.join("missing")).is_empty());
    }
}
