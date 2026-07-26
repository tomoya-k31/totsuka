//! Integration tests for the CLI command tree (#64), driving the real
//! `totsuka` binary with XDG pointed at a scratch directory.
//!
//! Covers the acceptance criteria: help/completion presence, `status` stale
//! reporting, cause+action error messages, and jq-parseable `--json` output.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use orchestrator_core::adapters::state_db::TaskMessageInsert;
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
    base_cmd(base).args(args).output().unwrap()
}

/// Run `totsuka <args>` with XDG dirs under `base` plus `TOTSUKA_*` overrides.
///
/// Inherited `TOTSUKA_*` vars are stripped first: an agent session running
/// these tests exports `TOTSUKA_JOB_ID` and friends, which would otherwise
/// leak into assertions about the env-override layer.
fn run_env(base: &Path, args: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut cmd = base_cmd(base);
    cmd.args(args);
    for (key, _) in std::env::vars() {
        if key.starts_with("TOTSUKA_") {
            cmd.env_remove(key);
        }
    }
    for (key, value) in vars {
        cmd.env(key, value);
    }
    cmd.output().unwrap()
}

fn base_cmd(base: &Path) -> Command {
    let mut cmd = Command::new(totsuka());
    cmd.env("XDG_CONFIG_HOME", base.join("cfg"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env("NO_COLOR", "1");
    cmd
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Seed a state DB with one task in each interesting state: running (with a
/// conversation), failed, and done.
fn seed_db(base: &Path) -> (i64, i64, i64) {
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
        last_signal_at: None,
    };
    let running = db.upsert_task(&new("1")).unwrap();
    db.apply_event(running, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(running, TaskEvent::Start, None).unwrap();
    db.record_session(running, "herdr", "sess-1").unwrap();
    // A conversation on the running task (#242): one message already handed
    // to the agent, one that arrived while it was working and is still queued.
    for (key, body, processed) in [
        ("1", "最初の質問です", true),
        (
            "1:reply",
            "追記: ログはこちらです\n\n    2026-07-26T00:00:00Z ERROR ...",
            false,
        ),
    ] {
        db.append_task_message(&TaskMessageInsert {
            task_id: running,
            message_key: key.to_string(),
            author: Some("アリス".to_string()),
            body: body.to_string(),
            url: Some(format!("https://slack.test/{key}")),
            payload: "{}".to_string(),
        })
        .unwrap();
        if processed {
            db.mark_messages_processed(running).unwrap();
        }
    }
    let failed = db.upsert_task(&new("2")).unwrap();
    db.apply_event(failed, TaskEvent::Fail, None).unwrap();
    let done = db.upsert_task(&new("3")).unwrap();
    for event in [
        TaskEvent::Dispatch,
        TaskEvent::Start,
        TaskEvent::BeginPublish,
        TaskEvent::Complete,
    ] {
        db.apply_event(done, event, None).unwrap();
    }
    (running, failed, done)
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
    let (running_id, _, _) = seed_db(&base);
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

/// #280: a task source lets a third party choose `title` / `body` / `author`
/// / `url` / `source_task_id`. Printed verbatim those can repaint the
/// operator's screen, so every human rendering has to defuse them — while
/// `--json` keeps the value byte-exact, because that is what an audit reads.
#[test]
fn external_text_cannot_repaint_the_terminal_yet_json_stays_verbatim() {
    let base = scratch("control_sequences");
    let state_dir = base.join("state").join("totsuka");
    std::fs::create_dir_all(&state_dir).unwrap();
    let db = StateDb::open(&state_dir.join("state.db")).unwrap();

    let esc = char::from_u32(0x1b).unwrap();
    // ESC[2J clears the screen; ESC[1A walks the cursor back over the row
    // already printed, which is how one task's row forges another's.
    let title = format!("{esc}[2Jinnocent{esc}[1A{esc}[2Kforged");
    // OSC 8: the text says one thing, the link goes somewhere else.
    let url = format!("{esc}]8;;https://evil.test{esc}\\https://slack.test/ok{esc}]8;;{esc}\\");
    // A bare CR rewrites the current row from column 0.
    let body = format!("please review\rTASK COMPLETED{esc}[2J");
    let author = format!("alice{esc}[31m");
    let source_task_id = format!("C123:{esc}[5m1700000000.000100");

    let id = db
        .upsert_task(&NewTask {
            source: "github".into(),
            source_task_id: source_task_id.clone(),
            workflow: "implement".into(),
            mode: "implement".into(),
            repo: Some("web".into()),
            priority: 0,
            title: title.clone(),
            url: Some(url.clone()),
            source_payload: None,
            last_signal_at: None,
        })
        .unwrap();
    db.append_task_message(&TaskMessageInsert {
        task_id: id,
        message_key: "m1".to_string(),
        author: Some(author.clone()),
        body: body.clone(),
        url: Some("https://slack.test/m1".to_string()),
        payload: "{}".to_string(),
    })
    .unwrap();

    // --- the human renderings must not carry a live escape ---------------
    for args in [
        vec!["task", "show", &id.to_string()],
        vec!["task", "list"],
        vec!["status"],
    ] {
        let out = run(&base, &args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        let text = stdout(&out);
        assert!(!text.contains(esc), "{args:?} emitted a live ESC: {text:?}");
        assert!(!text.contains('\r'), "{args:?} emitted a bare CR: {text:?}");
        // Neutralised, not deleted: the operator can still see what was sent.
        assert!(
            text.contains("innocent") && text.contains("forged"),
            "{args:?} swallowed the payload text: {text}"
        );
    }

    // One task seeded, so `task list` prints exactly one row under the
    // header — an escape must not be able to invent or erase rows.
    let listed = stdout(&run(&base, &["task", "list"]));
    assert_eq!(
        listed.lines().count(),
        2,
        "header + exactly one row: {listed:?}"
    );

    // --- --json keeps the original bytes, escaped once by serde_json -----
    let out = run(&base, &["task", "show", &id.to_string(), "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let raw = stdout(&out);
    // The document itself is transport-safe: no live control byte on the
    // wire, because serde_json wrote them as \uXXXX escapes.
    assert!(!raw.contains(esc), "raw JSON carried a live ESC: {raw:?}");

    let doc: serde_json::Value = serde_json::from_str(&raw).expect("jq-parseable JSON");
    // Decoded, every value is exactly what the source sent — no truncation,
    // no double-escaping, no sanitiser leaking across from the human path.
    assert_eq!(doc["title"].as_str().unwrap(), title);
    assert_eq!(doc["url"].as_str().unwrap(), url);
    assert_eq!(doc["source_task_id"].as_str().unwrap(), source_task_id);
    assert_eq!(doc["messages"][0]["body"].as_str().unwrap(), body);
    assert_eq!(doc["messages"][0]["author"].as_str().unwrap(), author);

    let listed: serde_json::Value =
        serde_json::from_str(&stdout(&run(&base, &["task", "list", "--json"]))).unwrap();
    assert_eq!(listed[0]["title"].as_str().unwrap(), title);

    let status: serde_json::Value =
        serde_json::from_str(&stdout(&run(&base, &["status", "--json"]))).unwrap();
    assert_eq!(status["tasks"][0]["title"].as_str().unwrap(), title);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn task_show_json_and_retry_cancel_rules() {
    let base = scratch("task");
    let (running_id, failed_id, done_id) = seed_db(&base);

    let out = run(&base, &["task", "show", &running_id.to_string(), "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["state"], "running");
    assert_eq!(doc["sessions"][0]["session_id"], "sess-1");
    assert!(
        doc["events"].as_array().unwrap().len() >= 3,
        "ingest + dispatch + start history: {doc}"
    );

    // The conversation (#242): every message, oldest first, with the whole
    // body (only the terminal rendering clips) and its processed state.
    let messages = doc["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "{doc}");
    assert_eq!(messages[0]["message_key"], "1");
    assert_eq!(messages[0]["author"], "アリス");
    assert_eq!(messages[0]["body"], "最初の質問です");
    assert!(
        !messages[0]["processed_at"].is_null(),
        "the first message went to the agent: {doc}"
    );
    assert_eq!(messages[1]["message_key"], "1:reply");
    assert!(
        messages[1]["body"]
            .as_str()
            .unwrap()
            .contains("2026-07-26T00:00:00Z ERROR"),
        "the JSON carries the full body, unclipped: {doc}"
    );
    assert!(
        messages[1]["processed_at"].is_null(),
        "the follow-up is still queued: {doc}"
    );

    // The human rendering shows the same conversation, flagging what is
    // still queued.
    let out = run(&base, &["task", "show", &running_id.to_string()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("conversation (2 message(s)"), "{text}");
    assert!(text.contains("1 not yet sent to the agent"), "{text}");
    assert!(text.contains("最初の質問です"), "{text}");
    // Multi-line bodies collapse to one line each.
    assert!(
        text.lines().filter(|l| l.contains("追記:")).count() == 1
            && !text.contains("    2026-07-26T00:00:00Z"),
        "a multi-line body is flattened into its one row: {text}"
    );

    // A task with no ledger rows (a source with one message per task) still
    // renders — the section is simply absent.
    let out = run(&base, &["task", "show", &failed_id.to_string()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(!stdout(&out).contains("conversation ("), "{}", stdout(&out));

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

    // A finished conversation is continued by another message, not by a
    // re-run — and `cancel` must give the same advice `retry` enforces,
    // which it did not before #242 (it pointed at a `retry` that refuses).
    for command in ["retry", "cancel"] {
        let out = run(&base, &["task", command, &done_id.to_string()]);
        assert!(
            !out.status.success(),
            "`task {command}` refuses a done task"
        );
        let err = stderr(&out);
        assert!(
            err.contains("another message in the conversation"),
            "`task {command}` must point at the way forward that works: {err}"
        );
        assert!(
            !err.contains("totsuka task retry"),
            "`task {command}` must not send a done task to a command that refuses it: {err}"
        );
    }

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

    // 同じ失敗を --json で: stderr は 1 行の JSON エンベロープになり、
    // 「原因 → アクション」が message / action フィールドに分かれる (#177)。
    let out = run(&base, &["status", "--json"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1), "runtime failure stays exit 1");
    let err = stderr(&out);
    let envelope: serde_json::Value =
        serde_json::from_str(err.trim()).expect("json error envelope parses");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("state database not found"),
        "{envelope}"
    );
    assert!(
        envelope["error"]["action"]
            .as_str()
            .unwrap()
            .contains("run"),
        "action field carries the next action: {envelope}"
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
    assert_eq!(
        out.status.code(),
        Some(3),
        "diagnostics that found problems exit 3, not the generic 1 (#177)"
    );
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
    // stderr は --json なので JSON エンベロープ。
    let envelope: serde_json::Value =
        serde_json::from_str(stderr(&out).trim()).expect("doctor json error envelope parses");
    assert_eq!(envelope["error"]["message"], "doctor found problems");

    // 非 --json でも exit code の意味は同じ（3）で、stderr は従来の平文。
    let out = run(&base, &["doctor"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("error: doctor found problems → follow the actions above"),
        "human-facing error keeps the → convention: {}",
        stderr(&out)
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

/// Seed a `base` with an empty `config.toml` so config-reading commands work.
fn seed_empty_config(base: &Path, contents: &str) {
    let cfg_dir = base.join("cfg/totsuka");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join("config.toml"), contents).unwrap();
}

/// A machine with no `XDG_STATE_HOME` — the macOS default — must still resolve
/// every state path from the XDG `$HOME/.local/state` fallback.
///
/// The regression this guards: the built-in worktree location used to be the
/// literal `"${XDG_STATE_HOME}/totsuka/worktrees/..."`, and `expand_env` treats
/// an unset variable as an error. `totsuka run` started fine and then failed
/// *every* dispatch at worktree creation, with nothing surfacing at startup.
#[test]
fn state_paths_resolve_without_xdg_state_home() {
    let base = scratch("no-xdg-state-home");
    seed_empty_config(&base, "");

    let mut cmd = Command::new(totsuka());
    // Deliberately no XDG_STATE_HOME. `HOME` points into the scratch dir so
    // the fallback cannot touch the developer's real home.
    cmd.env("XDG_CONFIG_HOME", base.join("cfg"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env_remove("XDG_STATE_HOME")
        .env("HOME", &base)
        .env("NO_COLOR", "1")
        .args(["doctor", "--json"]);
    for (key, _) in std::env::vars() {
        if key.starts_with("TOTSUKA_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd.output().unwrap();

    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("doctor --json parses");
    let checks = doc.as_array().unwrap();
    let check = |name: &str| {
        checks
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("{name} check present"))
    };

    let worktree = check("worktree-location");
    assert_eq!(
        worktree["ok"], true,
        "default location must resolve: {}",
        worktree["detail"]
    );
    // The state dir itself landed under the HOME fallback, not somewhere else.
    let state_db = check("state-db")["detail"].as_str().unwrap();
    assert!(
        state_db.contains(&base.join(".local/state/totsuka").display().to_string()),
        "state db must sit under the $HOME/.local/state fallback: {state_db}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #208: `TOTSUKA_*` must actually reach the config a command consumes — the
/// bug was that it was parsed nowhere and silently ignored.
#[test]
fn env_override_reaches_a_downstream_consumer() {
    let base = scratch("env-override");
    seed_empty_config(&base, "[hooks]\nsocket_path = \"/from/file.sock\"\n");

    // doctor's hook-socket check resolves [hooks].socket_path; with no
    // orchestrator running it reports the path it looked at.
    let out = run_env(
        &base,
        &["doctor", "--json"],
        &[("TOTSUKA_HOOKS_SOCKET_PATH", "/from/env.sock")],
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("doctor --json parses");
    let detail = doc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "hook-socket")
        .expect("hook-socket check present")["detail"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        detail.contains("/from/env.sock") && !detail.contains("/from/file.sock"),
        "env layer must beat config.toml: {detail}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// A bad value aborts with the variable named — the point of #208 is that a
/// broken override is never silent.
#[test]
fn invalid_env_override_fails_loudly() {
    let base = scratch("env-invalid");
    seed_empty_config(&base, "max_concurrency = 4\n");

    let out = run_env(
        &base,
        &["config", "validate", "--offline"],
        &[("TOTSUKA_MAX_CONCURRENCY", "abc")],
    );
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("TOTSUKA_MAX_CONCURRENCY") && err.contains("abc"),
        "error names the variable and the value: {err}"
    );

    // An unknown name only warns; the run continues.
    let out = run_env(
        &base,
        &["config", "validate", "--offline"],
        &[("TOTSUKA_MAX_CONCURENCY", "5")],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("TOTSUKA_MAX_CONCURENCY"),
        "typo is warned about: {}",
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// `config show` prints the files, so it must also disclose the env layer —
/// otherwise it misreports what the daemon will use.
#[test]
fn config_show_lists_active_env_overrides() {
    let base = scratch("env-show");
    seed_empty_config(&base, "");

    let out = run_env(
        &base,
        &["config", "show", "--redacted"],
        &[
            ("TOTSUKA_MAX_CONCURRENCY", "9"),
            ("TOTSUKA_HOOKS_AUTH_TOKEN_REF", "keychain:totsuka/hook"),
            // Reserved injection var: a different mechanism, not an override.
            ("TOTSUKA_JOB_ID", "job-1-2"),
            // Empty = unset, so it is not in effect and must not be listed.
            ("TOTSUKA_LOG_LEVEL", ""),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("active env overrides"), "{text}");
    assert!(text.contains("TOTSUKA_MAX_CONCURRENCY=9"), "{text}");
    assert!(
        !text.contains("TOTSUKA_JOB_ID"),
        "reserved var listed: {text}"
    );
    assert!(
        !text.contains("TOTSUKA_LOG_LEVEL"),
        "an empty value is ignored by apply_env_overrides, so listing it as \
         active would misreport the effective config: {text}"
    );
    assert!(
        !text.contains("keychain:totsuka/hook"),
        "--redacted masks a secret-looking name: {text}"
    );

    // Nothing set → no section at all.
    let out = run_env(&base, &["config", "show"], &[]);
    assert!(!stdout(&out).contains("active env overrides"));
    let _ = std::fs::remove_dir_all(&base);
}

/// Install a manifest into the plugin store. Only `plugin.toml` is written —
/// the hook-capability verdict is read from the static manifest, so no binary
/// is needed for the checks under test (the live `plugin:*` probes fail
/// without one, which is why the assertions below target the `hook-token`
/// check rather than doctor's overall exit code where it is not the point).
fn seed_manifest(base: &Path, name: &str, capabilities: &str) {
    let dir = base.join("data/totsuka/plugins").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "name = \"{name}\"\nkind = \"agent_ide\"\nversion = \"0.1.0\"\n\
             protocol_version = \"^0.2\"\n\n[capabilities]\n{capabilities}\n"
        ),
    )
    .unwrap();
}

/// #176: `--debug` is a global flag, so it must have an effect on every
/// command, not just `run` — a debug-level stderr subscriber, while the
/// stdout `--json` contract stays parseable (no log lines on stdout).
#[test]
fn debug_flag_enables_stderr_diagnostics_on_non_run_commands() {
    let base = scratch("debug-flag");
    seed_db(&base);

    let out = run(&base, &["status", "--json", "--debug"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    serde_json::from_str::<serde_json::Value>(&stdout(&out))
        .expect("stdout stays parseable JSON with --debug");
    assert!(
        stderr(&out).contains("verbose diagnostics enabled"),
        "--debug installs the stderr subscriber: {}",
        stderr(&out)
    );

    // Without the flag nothing changes.
    let out = run(&base, &["status", "--json"]);
    assert!(out.status.success());
    assert!(!stderr(&out).contains("verbose diagnostics enabled"));

    // Even `completion` (which needs no environment and returns early)
    // honors the flag — "every command" means every command.
    let out = run(&base, &["completion", "zsh", "--debug"]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains("totsuka"),
        "completion script still lands on stdout"
    );
    assert!(
        stderr(&out).contains("verbose diagnostics enabled"),
        "--debug takes effect on completion too: {}",
        stderr(&out)
    );

    // No log files are created for non-run commands (file logging is `run`'s).
    assert!(
        !base.join("state/totsuka/logs").exists(),
        "--debug on a non-run command must not create log files"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Install a `plugin.toml` that does not parse (#214).
fn seed_broken_manifest(base: &Path, name: &str) {
    let dir = base.join("data/totsuka/plugins").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), "this is not valid toml {{{\n").unwrap();
}

/// A config whose single workflow drives `agent`, with `[hooks]` left without
/// an `auth_token_ref`.
fn hook_config(agent: &str) -> String {
    format!(
        "[plugins.src]\nenabled = true\nkind = \"task_source\"\n\n\
         [plugins.{agent}]\nenabled = true\nkind = \"agent_ide\"\n\n\
         [[workflows]]\nname = \"wf\"\nsource = \"src\"\nmode = \"implement\"\n\
         agent = \"{agent}\"\noutput = \"none\"\nverification = \"none\"\n"
    )
}

/// #209: an unset `[hooks].auth_token_ref` used to pass silently — the
/// validate warning was unreachable (`|_| None`) and doctor only warned. With
/// a hook-capable agent in play it must now fail doctor outright.
#[test]
fn unset_hook_token_fails_doctor_when_an_agent_is_hook_capable() {
    let base = scratch("hook-token-fail");
    seed_empty_config(&base, &hook_config("herdr"));
    seed_manifest(&base, "herdr", "resume_session = true");

    let out = run(&base, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(3), "problems found exit 3 (#177)");
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("doctor --json parses");
    let check = doc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "hook-token")
        .expect("hook-token check present")
        .clone();
    assert_eq!(check["ok"], false, "{check}");
    let detail = check["detail"].as_str().unwrap();
    assert!(
        detail.contains("`wf`") && detail.contains("`herdr`"),
        "detail names the offending workflow and agent: {detail}"
    );
    assert!(check["action"].as_str().unwrap().contains("auth_token_ref"));
    let _ = std::fs::remove_dir_all(&base);
}

/// The same omission stays advisory when no workflow uses a hook-capable
/// agent: that config never needs the token, and the 0600 socket still guards
/// the receiver.
#[test]
fn unset_hook_token_stays_advisory_without_a_hook_capable_agent() {
    let base = scratch("hook-token-warn");
    seed_empty_config(&base, &hook_config("orca"));
    // orca declares neither 0.1.3 flag.
    seed_manifest(&base, "orca", "plan_mode = true");

    let out = run(&base, &["doctor", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("doctor --json parses");
    let check = doc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "hook-token")
        .expect("hook-token check present")
        .clone();
    assert_eq!(check["ok"], true, "{check}");
    assert_eq!(check["warning"], true, "{check}");
    let _ = std::fs::remove_dir_all(&base);
}

/// `config validate` keeps exiting 0, but the warning that #209 found
/// unreachable now actually prints.
#[test]
fn unset_hook_token_warns_in_config_validate() {
    let base = scratch("hook-token-validate");
    seed_empty_config(&base, &hook_config("herdr"));
    seed_manifest(&base, "herdr", "diagnostics_snapshot = true");

    let out = run(&base, &["config", "validate", "--offline"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("warning:")
            && text.contains("auth_token_ref")
            && text.contains("`wf`")
            && text.contains("`herdr`"),
        "the hook-token warning must fire: {text}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #214: an installed-but-unparsable `plugin.toml` used to pass
/// `config validate --offline` without a single diagnostic — and, worse,
/// silently skipped the capability-based advisories. It must now be an error
/// (exit non-zero), offline included.
#[test]
fn broken_manifest_fails_config_validate_offline() {
    let base = scratch("broken-manifest-validate");
    seed_empty_config(&base, &hook_config("herdr"));
    seed_broken_manifest(&base, "herdr");

    let out = run(&base, &["config", "validate", "--offline"]);
    assert!(
        !out.status.success(),
        "a broken manifest must fail validation: {}",
        stdout(&out)
    );
    let text = stdout(&out);
    assert!(
        text.contains("error:") && text.contains("`herdr`") && text.contains("plugin.toml"),
        "the error names the plugin and the manifest: {text}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #214 (doctor side): when an agent's manifest cannot be parsed, its hook
/// capability is *unknown*, not "no" — the `hook-token` advisory must say so
/// instead of silently downgrading what would be a failure with a readable
/// manifest.
#[test]
fn broken_manifest_marks_hook_capability_unknown_in_doctor() {
    let base = scratch("broken-manifest-doctor");
    seed_empty_config(&base, &hook_config("herdr"));
    seed_broken_manifest(&base, "herdr");

    let out = run(&base, &["doctor", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "the broken manifest fails doctor"
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("doctor --json parses");
    let check = doc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "hook-token")
        .expect("hook-token check present")
        .clone();
    assert_eq!(
        check["ok"], true,
        "still advisory, not a hard fail: {check}"
    );
    assert_eq!(check["warning"], true, "{check}");
    let detail = check["detail"].as_str().unwrap();
    assert!(
        detail.contains("unknown") && detail.contains("`wf`") && detail.contains("`herdr`"),
        "the warn names the workflow whose agent capability is unknown: {detail}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// A canned HTTP endpoint standing in for an OpenAI-compatible gateway.
///
/// Returns its base URL and a counter of the requests it served, so a test
/// can assert not only what `doctor` reported but whether it called out at
/// all — the whole point of `--online` being opt-in.
fn mock_llm_gateway(status: u16, reason: &str, body: &'static str) -> (String, Arc<AtomicUsize>) {
    use std::io::{Read, Write};

    /// How long a mock keeps listening. Generous next to the sub-second
    /// probe, but bounded so no mock outlives the test that made it.
    const MOCK_GATEWAY_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    // Non-blocking + a deadline rather than a blocking `incoming()`: the
    // opt-in assertion below deliberately sends *nothing*, and a blocking
    // accept would park this thread (holding the port) for the rest of the
    // test binary's run.
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&served);
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + MOCK_GATEWAY_LIFETIME;
        while std::time::Instant::now() < deadline {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(_) => break,
            };
            // The accepted socket inherits O_NONBLOCK on BSD/macOS but not on
            // Linux, so pin it explicitly; a read timeout keeps a client that
            // connects and says nothing from stalling the thread.
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            // The probe request is a few hundred bytes; one read drains it,
            // so closing the socket cannot reset the client mid-response.
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}/v1"), served)
}

/// Read one check out of `doctor --json` output.
fn doctor_check(out: &Output, name: &str) -> Option<serde_json::Value> {
    let doc: serde_json::Value = serde_json::from_str(&stdout(out)).expect("doctor --json parses");
    doc.as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .cloned()
}

/// `doctor` alone can only say the key *reference* resolves; `--online` is
/// what proves the provider accepts it (#267). A rejected key must fail the
/// check — that is the whole gap the flag closes.
#[test]
fn doctor_online_reports_a_rejected_llm_key() {
    let base = scratch("doctor-online-401");
    let (url, served) = mock_llm_gateway(
        401,
        "Unauthorized",
        r#"{"error":{"message":"User not found.","code":401}}"#,
    );
    seed_empty_config(
        &base,
        &format!(
            "[llm]\nbase_url = \"{url}\"\nmodel = \"probe-model\"\n\
             api_key_ref = \"${{DOCTOR_TEST_LLM_KEY}}\"\n"
        ),
    );
    let env = [("DOCTOR_TEST_LLM_KEY", "sk-dead")];

    // Offline: the reference resolves, so doctor is happy — and silent about
    // the key being dead. No request left the process.
    let offline = run_env(&base, &["doctor", "--json"], &env);
    let llm = doctor_check(&offline, "llm").expect("llm check present");
    assert_eq!(llm["ok"], true, "{llm}");
    assert!(
        doctor_check(&offline, "llm-online").is_none(),
        "the live probe must not run without --online"
    );
    assert_eq!(
        served.load(Ordering::SeqCst),
        0,
        "plain doctor stays offline"
    );

    // Online: the same config now fails, naming the provider's own words.
    let online = run_env(&base, &["doctor", "--json", "--online"], &env);
    let probe = doctor_check(&online, "llm-online").expect("llm-online check present");
    assert_eq!(probe["ok"], false, "a rejected key fails doctor: {probe}");
    assert!(
        probe["detail"]
            .as_str()
            .unwrap()
            .contains("User not found."),
        "the provider's message reaches the operator: {probe}"
    );
    assert!(
        probe["action"].as_str().unwrap().contains("api_key_ref"),
        "cause + next action (§7): {probe}"
    );
    assert_eq!(
        served.load(Ordering::SeqCst),
        1,
        "exactly one probe, no retries"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// An accepted key passes, and a provider that is merely unwell stays
/// advisory: a 5xx says nothing about the credentials, so it must not turn
/// `doctor` red.
#[test]
fn doctor_online_passes_on_2xx_and_only_warns_on_5xx() {
    let base = scratch("doctor-online-2xx");
    let (ok_url, _) = mock_llm_gateway(200, "OK", r#"{"choices":[{"message":{"content":"x"}}]}"#);
    seed_empty_config(
        &base,
        &format!("[llm]\nbase_url = \"{ok_url}\"\nmodel = \"probe-model\"\n"),
    );

    // No `api_key_ref` at all — a keyless local gateway still gets probed.
    let out = run(&base, &["doctor", "--json", "--online"]);
    let probe = doctor_check(&out, "llm-online").expect("llm-online check present");
    assert_eq!(probe["ok"], true, "{probe}");
    assert!(
        probe["detail"].as_str().unwrap().contains(&ok_url),
        "the probed endpoint is named: {probe}"
    );

    let (bad_url, _) = mock_llm_gateway(503, "Service Unavailable", "upstream is down");
    seed_empty_config(
        &base,
        &format!("[llm]\nbase_url = \"{bad_url}\"\nmodel = \"probe-model\"\n"),
    );
    let out = run(&base, &["doctor", "--json", "--online"]);
    let probe = doctor_check(&out, "llm-online").expect("llm-online check present");
    assert_eq!(probe["ok"], true, "a 5xx must not fail doctor: {probe}");
    assert_eq!(probe["warning"], true, "{probe}");
    let _ = std::fs::remove_dir_all(&base);
}

/// `doctor` surfaces the schema version and the release that applied it
/// (#275) — otherwise it takes a manual `sqlite3` to answer "which schema is
/// this DB on" after an upgrade or a rollback.
#[test]
fn doctor_reports_the_state_db_schema_version() {
    let base = scratch("doctor_schema");
    seed_db(&base);

    let out = run(&base, &["doctor", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("doctor --json parses");
    let check = doc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "state-db")
        .expect("state-db check present")
        .clone();
    assert_eq!(check["ok"], true, "{check}");
    let detail = check["detail"].as_str().unwrap();
    assert!(
        detail.contains("schema v"),
        "the schema version is on the state-db line: {detail}"
    );
    assert!(
        detail.contains(&format!("applied by {}", env!("CARGO_PKG_VERSION"))),
        "…along with the release that applied it: {detail}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A state.db from a newer totsuka stops the read-only commands too, with an
/// error naming the release to upgrade to (#275). Before the guard, the
/// unknown schema was simply used as-is.
#[test]
fn a_newer_state_db_is_refused_with_an_upgrade_hint() {
    let base = scratch("downgrade");
    seed_db(&base);

    // Forge a version this binary cannot know, attributed to a future release.
    let db_path = base.join("state/totsuka/state.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let next: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM schema_migrations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at, applied_by) \
             VALUES (?1, '2026-08-10T00:00:00Z', '9.9.9')",
            rusqlite::params![next],
        )
        .unwrap();
    }

    // Read-only path: `status` must stop rather than read an unknown schema.
    let out = run(&base, &["status"]);
    assert_ne!(out.status.code(), Some(0), "status must not succeed");
    assert!(
        stderr(&out).contains("9.9.9"),
        "the error names the release to upgrade to: {}",
        stderr(&out)
    );

    // And doctor reports it as a failing check with exit 3, not a crash.
    let out = run(&base, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("doctor --json parses");
    let check = doc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "state-db")
        .expect("state-db check present")
        .clone();
    assert_eq!(check["ok"], false, "{check}");
    assert!(
        check["detail"].as_str().unwrap().contains("9.9.9"),
        "{check}"
    );

    let _ = std::fs::remove_dir_all(&base);
}
