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
        thread_key: None,
        last_signal_at: None,
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
