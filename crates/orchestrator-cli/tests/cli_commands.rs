//! Integration tests for the CLI command tree (#64), driving the real
//! `totsuka` binary with XDG pointed at a scratch directory.
//!
//! Covers the acceptance criteria: help/completion presence, `status` stale
//! reporting, cause+action error messages, and jq-parseable `--json` output.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orchestrator_core::adapters::{NewTask, StateDb};
use orchestrator_core::domain::state::TaskEvent;

fn totsuka() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_totsuka"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("totsuka-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `totsuka <args>` with XDG dirs under `base`.
fn run(base: &Path, args: &[&str]) -> Output {
    Command::new(totsuka())
        .args(args)
        .env("XDG_CONFIG_HOME", base.join("cfg"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Seed a state DB with one task in each interesting state.
fn seed_db(base: &Path) -> (i64, i64) {
    let state_dir = base.join("state").join("totsuka");
    std::fs::create_dir_all(&state_dir).unwrap();
    let db = StateDb::open(&state_dir.join("state.db")).unwrap();
    let new = |id: &str| NewTask {
        source: "github".into(),
        source_task_id: id.into(),
        workflow: "implement".into(),
        mode: "implement".into(),
        repo: Some("web".into()),
        priority: 0,
        title: format!("task {id}"),
        url: None,
        source_payload: None,
    };
    let running = db.upsert_task(&new("1")).unwrap();
    db.apply_event(running, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(running, TaskEvent::Start, None).unwrap();
    db.record_session(running, "herdr", "sess-1").unwrap();
    let failed = db.upsert_task(&new("2")).unwrap();
    db.apply_event(failed, TaskEvent::Fail, None).unwrap();
    (running, failed)
}

#[test]
fn help_lists_every_command_and_completion_generates() {
    let base = scratch("help");
    let help = run(&base, &["--help"]);
    assert!(help.status.success());
    let text = stdout(&help);
    for command in [
        "init",
        "run",
        "status",
        "task",
        "plugin",
        "config",
        "logs",
        "doctor",
        "completion",
    ] {
        assert!(text.contains(command), "help must list `{command}`: {text}");
    }

    for shell in ["zsh", "bash"] {
        let completion = run(&base, &["completion", shell]);
        assert!(completion.status.success(), "completion {shell} works");
        assert!(
            stdout(&completion).contains("totsuka"),
            "completion output mentions the binary"
        );
    }

    // No subcommand is a usage error (exit code 2, one message) distinct from
    // a runtime failure (exit 1).
    let none = run(&base, &[]);
    assert_eq!(
        none.status.code(),
        Some(2),
        "no-command usage error exits 2, not 1"
    );
    let err = stderr(&none);
    assert!(err.contains("no command given"));
    assert!(
        !err.contains("error:"),
        "single usage line, not a doubled error: {err}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn status_reports_stale_lock_and_parseable_json() {
    let base = scratch("status");
    let (running_id, _) = seed_db(&base);
    // A lock file with a certainly-dead PID → stale (F-74).
    std::fs::write(base.join("state/totsuka/run.lock"), "999999").unwrap();

    let out = run(&base, &["status"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("not running") && text.contains("stale"),
        "stale lock must be explicit: {text}"
    );

    let out = run(&base, &["status", "--json"]);
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("jq-parseable JSON");
    assert_eq!(doc["orchestrator"]["running"], false);
    assert_eq!(doc["orchestrator"]["stale_lock"], true);
    assert!(
        doc["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == running_id && t["state"] == "running"),
        "seeded running task appears: {doc}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn task_show_json_and_retry_cancel_rules() {
    let base = scratch("task");
    let (running_id, failed_id) = seed_db(&base);

    let out = run(&base, &["task", "show", &running_id.to_string(), "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["state"], "running");
    assert_eq!(doc["sessions"][0]["session_id"], "sess-1");
    assert!(
        doc["events"].as_array().unwrap().len() >= 3,
        "ingest + dispatch + start history: {doc}"
    );

    // Retry only re-queues failed/cancelled tasks.
    let out = run(&base, &["task", "retry", &running_id.to_string()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("→"), "cause+action: {}", stderr(&out));
    let out = run(&base, &["task", "retry", &failed_id.to_string()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    // Cancel works on the (re-queued) task; a second cancel is refused.
    let out = run(&base, &["task", "cancel", &failed_id.to_string()]);
    assert!(out.status.success());
    let out = run(&base, &["task", "cancel", &failed_id.to_string()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("→"));

    // Unknown id → cause + action.
    let out = run(&base, &["task", "show", "999"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("task list"), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn missing_config_and_db_errors_have_cause_and_action() {
    let base = scratch("errors");

    // config 不在 (config validate).
    let out = run(&base, &["config", "validate"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("config not found") && err.contains("→") && err.contains("totsuka init"),
        "cause+action for missing config: {err}"
    );

    // DB 不在 (status).
    let out = run(&base, &["status"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("state database not found") && err.contains("→"),
        "cause+action for missing db: {err}"
    );

    // プラグイン不在 (enabled but not installed) via config validate --offline
    // then doctor --json.
    let cfg_dir = base.join("cfg/totsuka");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[plugins.ghost]\nenabled = true\nkind = \"task_source\"\n",
    )
    .unwrap();
    let out = run(&base, &["doctor", "--json"]);
    assert!(!out.status.success());
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("doctor --json parses");
    let ghost = doc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "plugin:ghost")
        .expect("ghost plugin check present");
    assert_eq!(ghost["ok"], false);
    assert!(
        ghost["detail"].as_str().unwrap().contains("not installed"),
        "{ghost}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn init_creates_skeleton_and_never_overwrites() {
    let base = scratch("init");
    let out = run(&base, &["init"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let config_path = base.join("cfg/totsuka/config.toml");
    assert!(config_path.exists());

    // Re-running must not clobber user edits.
    std::fs::write(&config_path, "# my edits\n").unwrap();
    let out = run(&base, &["init"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("skipped"));
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        "# my edits\n"
    );

    // The generated skeleton passes offline validation once uncommented-free.
    std::fs::remove_file(&config_path).unwrap();
    run(&base, &["init"]);
    let out = run(&base, &["config", "validate", "--offline"]);
    assert!(
        out.status.success(),
        "generated skeleton must validate: {}",
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn config_show_redacts_secret_keys() {
    let base = scratch("redact");
    let cfg_dir = base.join("cfg/totsuka");
    std::fs::create_dir_all(cfg_dir.join("plugins")).unwrap();
    std::fs::write(cfg_dir.join("config.toml"), "").unwrap();
    std::fs::write(
        cfg_dir.join("plugins/github.toml"),
        "github_token = \"ghp_secret_value\"\nname = \"visible\"\n",
    )
    .unwrap();

    let out = run(&base, &["config", "show", "--redacted"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains("ghp_secret_value"), "secret masked: {text}");
    assert!(text.contains("***redacted***"));
    assert!(text.contains("visible"));
    let _ = std::fs::remove_dir_all(&base);
}
