//! `totsuka logs` — human-friendly view over the JSON Lines logs (§5.1/§5.2):
//! formatting, `-f` follow, and `--task <id>` filtering.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

use orchestrator_core::logging;
use serde_json::Value;

use crate::common::{CliError, Cx};

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

    let mut file = std::fs::File::open(&path)?;
    let mut reader = BufReader::new(&mut file);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        print_line(&line, task);
    }

    if !follow {
        return Ok(());
    }
    // Follow: poll for appended bytes (daily rotation means a fresh file may
    // appear at midnight; re-resolve the newest file on every tick).
    let mut offset = file.stream_position()?;
    let mut current = path;
    loop {
        std::thread::sleep(FOLLOW_TICK);
        if let Some(newest) = latest_log_file(&dir)?
            && newest != current
        {
            current = newest;
            offset = 0;
        }
        let mut file = std::fs::File::open(&current)?;
        let len = file.metadata()?.len();
        if len <= offset {
            continue;
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(&mut file);
        let mut line = String::new();
        while reader.read_line(&mut line)? > 0 {
            print_line(&line, task);
            line.clear();
        }
        offset = len;
    }
}

/// The lexically-newest `totsuka.log.*` file (dates sort chronologically).
fn latest_log_file(dir: &std::path::Path) -> Result<Option<PathBuf>, CliError> {
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
        println!("{trimmed}");
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
    println!("{timestamp} {level:<5} {message}{extras}");
}
