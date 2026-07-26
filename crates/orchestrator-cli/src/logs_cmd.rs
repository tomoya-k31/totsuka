//! `totsuka logs` — human-friendly view over the JSON Lines logs (§5.1/§5.2):
//! formatting, `-f` follow, and `--task <id>` filtering.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use orchestrator_core::logging;
use serde_json::Value;

use crate::common::{CliError, Cx, safe};

/// Poll interval for `-f`.
const FOLLOW_TICK: Duration = Duration::from_millis(500);

/// Execute `totsuka logs`.
pub fn run(cx: &Cx, follow: bool, task: Option<i64>) -> Result<(), CliError> {
    let dir = logging::default_log_dir(cx.paths.state_dir());
    let Some(path) = latest_log_file(&dir)? else {
        return Err(format!(
            "no log files under {} → `totsuka run` writes them",
            dir.display()
        )
        .into());
    };

    // Print the whole current file, then follow from its end.
    let mut reader = BufReader::new(std::fs::File::open(&path)?);
    drain(&mut reader, task)?;

    if !follow {
        return Ok(());
    }
    // Follow with a persistent handle: each tick reads only the bytes appended
    // since the last read (the reader's own position is the source of truth, so
    // a line written *during* a drain is not re-printed next tick). The log
    // directory is re-scanned for a rotated file only on a tick that produced
    // nothing new — so a steady stream costs no readdir, and the previous
    // day's tail is fully drained before switching (no lost lines).
    let mut current = path;
    loop {
        std::thread::sleep(FOLLOW_TICK);
        let advanced = drain(&mut reader, task)?;
        if !advanced
            && let Some(newest) = latest_log_file(&dir)?
            && newest != current
        {
            current = newest;
            reader = BufReader::new(std::fs::File::open(&current)?);
        }
    }
}

/// Read and print every complete line available from `reader` up to its current
/// EOF, leaving the reader positioned exactly at the bytes consumed. Returns
/// whether any line was printed.
fn drain<R: BufRead>(reader: &mut R, task: Option<i64>) -> Result<bool, CliError> {
    let mut line = String::new();
    let mut advanced = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break; // EOF for now; a growing file yields more on the next drain.
        }
        print_line(&line, task);
        advanced = true;
    }
    Ok(advanced)
}

/// The lexically-newest `totsuka.log.*` file (dates sort chronologically).
fn latest_log_file(dir: &Path) -> Result<Option<PathBuf>, CliError> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(None);
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("totsuka.log"))
        })
        .collect();
    files.sort();
    Ok(files.pop())
}

/// Format one JSON log line for humans; pass through unparseable lines.
fn print_line(line: &str, task: Option<i64>) {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        // Not our JSON — a partially flushed write, or something else
        // appending to the file. Least trustworthy line in the whole command,
        // so it is the last one that should reach the terminal raw (#280).
        println!("{}", safe(trimmed));
        return;
    };
    if let Some(wanted) = task {
        let matches = value
            .get("task_id")
            .is_some_and(|v| v.as_i64() == Some(wanted) || v.as_str() == Some(&wanted.to_string()));
        if !matches {
            return;
        }
    }
    let timestamp = value["timestamp"].as_str().unwrap_or("-");
    let level = value["level"].as_str().unwrap_or("-");
    let message = value["message"].as_str().unwrap_or("");
    let mut extras = String::new();
    if let Some(obj) = value.as_object() {
        for (key, val) in obj {
            if matches!(key.as_str(), "timestamp" | "level" | "message" | "target") {
                continue;
            }
            extras.push_str(&format!(" {key}={val}"));
        }
    }
    // `message` is the one that matters: the log *file* holds control
    // characters safely escaped (serde_json wrote each line, so an ESC is
    // stored as the six ASCII characters of a unicode escape), and `as_str`
    // above decodes them back into a live byte. Reading a log must not be the
    // step that re-arms them (#280).
    //
    // `extras` is already safe by construction — it formats each `Value` with
    // `Display`, i.e. re-serialises to JSON, which re-escapes. That is a
    // property of how it is built, not a guarantee, so it is re-checked here
    // rather than trusted; `safe` borrows when there is nothing to do.
    println!(
        "{} {:<5} {}{}",
        safe(timestamp),
        safe(level),
        safe(message),
        safe(&extras)
    );
}
