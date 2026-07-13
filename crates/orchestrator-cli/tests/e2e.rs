//! End-to-end tests (#66, §9): drive the real `totsuka` **binary** through the
//! whole flow against real mock-plugin subprocesses and a real git repository.
//!
//! These complement the engine-level integration tests in
//! `orchestrator-core/tests/run_loop.rs` by exercising the CLI wiring — config
//! load, plugin launch from the store, logging, the run lock, and the
//! `run`/`status`/`task` commands — as a user would.
//!
//! Flake control: every run is **one-shot** (deterministic, no `--watch`
//! timing) and wrapped in a wall-clock guard; poll intervals are irrelevant to
//! one-shot runs.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use test_support::scratch;

/// Path to the compiled `totsuka` binary.
fn totsuka() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_totsuka"))
}

/// Path to the `mock_plugin` binary (a bin of `orchestrator-core`, built at the
/// same target dir under the same profile as the tests). Rebuilt every call so
/// an edit to `mock_plugin.rs` is never silently missed — cargo's dependency
/// tracking makes it a fast no-op when already fresh.
fn mock_plugin() -> PathBuf {
    // `CARGO_BIN_EXE_totsuka` lives in the same profile dir the tests use; its
    // parent's name is the profile (`debug` / `release`).
    let bin_dir = totsuka().parent().expect("target dir").to_path_buf();
    let mut build = Command::new(env!("CARGO"));
    build.args(["build", "-p", "orchestrator-core", "--bin", "mock_plugin"]);
    if bin_dir.file_name().and_then(|n| n.to_str()) == Some("release") {
        build.arg("--release");
    }
    let status = build.status().expect("spawn cargo build for mock_plugin");
    assert!(status.success(), "failed to build mock_plugin");

    let path = bin_dir.join(format!("mock_plugin{}", std::env::consts::EXE_SUFFIX));
    assert!(path.exists(), "mock_plugin not found at {}", path.display());
    path
}

/// The XDG-scoped environment for a scratch base.
struct Env {
    base: PathBuf,
    repo: PathBuf,
    source_log: PathBuf,
    notify_log: PathBuf,
}

impl Env {
    /// XDG dirs get a `totsuka` suffix; place files accordingly.
    fn cfg_dir(&self) -> PathBuf {
        self.base.join("cfg/totsuka")
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state/totsuka")
    }
    fn plugins_store(&self) -> PathBuf {
        self.base.join("data/totsuka/plugins")
    }

    /// Run `totsuka <args>` with XDG pointed at the scratch dirs and a wall
    /// clock guard so a hang fails fast instead of stalling CI. stdout/stderr
    /// are drained by dedicated threads while we poll, so a chatty child can
    /// never deadlock on a full pipe, and a timed-out child is killed (not
    /// leaked as an orphan holding the run lock).
    fn run(&self, args: &[&str]) -> Output {
        let start = Instant::now();
        let mut child = Command::new(totsuka())
            .args(args)
            .env("XDG_CONFIG_HOME", self.base.join("cfg"))
            .env("XDG_DATA_HOME", self.base.join("data"))
            .env("XDG_STATE_HOME", self.base.join("state"))
            .env("XDG_CACHE_HOME", self.base.join("cache"))
            .env("NO_COLOR", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let mut out_pipe = child.stdout.take().unwrap();
        let mut err_pipe = child.stderr.take().unwrap();
        let out_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out_pipe.read_to_end(&mut buf);
            buf
        });
        let err_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err_pipe.read_to_end(&mut buf);
            buf
        });

        // One-shot runs settle quickly; guard against a regression that hangs.
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() >= Duration::from_secs(60) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`totsuka {args:?}` did not finish within 60s (killed)");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Output {
            status,
            stdout: out_reader.join().unwrap(),
            stderr: err_reader.join().unwrap(),
        }
    }
}

/// Install the mock plugin binary as `name` (kind `kind`) into the store.
fn install_plugin(env: &Env, name: &str, kind: &str) {
    let dir = env.plugins_store().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(mock_plugin(), dir.join(name)).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "name = \"{name}\"\nkind = \"{kind}\"\nversion = \"0.1.0\"\n\
             protocol_version = \"^0.1\"\n\n[capabilities]\nstate_stream = true\n\
             outputs = [\"source\"]\n"
        ),
    )
    .unwrap();
}

/// Set up an XDG scratch env: git bare origin + clone, 3 installed mock
/// plugins, config.toml, and the plugin configs. `agent_cfg` injects the mock
/// agent scenario; `output` picks the workflow output policy.
fn setup(name: &str, agent_cfg: &str, output: &str, mode: &str) -> Env {
    let base = scratch(name);
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    // bare origin + clone with one commit on main (shared helper).
    test_support::bare_origin_and_clone(&repo);

    let env = Env {
        source_log: base.join("source.ndjson"),
        notify_log: base.join("notify.ndjson"),
        base,
        repo: repo.clone(),
    };

    let cfg_dir = env.cfg_dir();
    std::fs::create_dir_all(cfg_dir.join("plugins")).unwrap();
    std::fs::create_dir_all(env.state_dir()).unwrap();

    install_plugin(&env, "mock_src", "task_source");
    install_plugin(&env, "mock_agent", "agent_ide");
    install_plugin(&env, "mock_notify", "notifier");

    std::fs::write(
        cfg_dir.join("config.toml"),
        format!(
            r#"
[plugins.mock_src]
enabled = true
kind = "task_source"

[plugins.mock_agent]
enabled = true
kind = "agent_ide"

[plugins.mock_notify]
enabled = true
kind = "notifier"

[[repositories]]
name = "clone"
path = "{clone}"

[worktree]
location = "{state}/wt/{{repo_name}}/{{branch}}"
cleanup = "immediate"
plan_cleanup = "immediate"

[[workflows]]
name = "wf"
source = "mock_src"
trigger = {{}}
mode = "{mode}"
agent = "mock_agent"
output = "{output}"
on_success = {{ set_status = "レビュー待ち" }}
"#,
            clone = env.repo.join("clone").display(),
            state = env.state_dir().display(),
        ),
    )
    .unwrap();

    std::fs::write(
        cfg_dir.join("plugins/mock_src.toml"),
        format!(
            "notify_log = \"{}\"\n[[tasks]]\nid = \"1\"\nsource = \"mock_src\"\ntitle = \"e2e task\"\n",
            env.source_log.display()
        ),
    )
    .unwrap();
    std::fs::write(cfg_dir.join("plugins/mock_agent.toml"), agent_cfg).unwrap();
    std::fs::write(
        cfg_dir.join("plugins/mock_notify.toml"),
        format!("notify_log = \"{}\"\n", env.notify_log.display()),
    )
    .unwrap();

    env
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Read a recorded NDJSON log (empty if never written).
fn read_log(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn e2e_full_path_source_output_binary() {
    let env = setup(
        "happy",
        "stream_states = [\"running\", \"done\"]\n",
        "source",
        "plan",
    );

    // One-shot run drives fetch → dispatch → done → publish → cleanup.
    let out = env.run(&["run"]);
    assert!(out.status.success(), "run failed: {}", stdout(&out));
    assert!(
        stdout(&out).contains("done 1"),
        "summary reports done: {}",
        stdout(&out)
    );

    // The result artifact reached the source plugin (F-07).
    let source_calls = read_log(&env.source_log);
    assert!(
        source_calls.iter().any(|c| c["method"] == "result/publish"),
        "result/publish recorded: {source_calls:?}"
    );
    // The notifier saw the done event (F-90).
    assert!(
        read_log(&env.notify_log)
            .iter()
            .any(|n| n["params"]["event"] == "done"),
        "done notification delivered"
    );

    // `status --json` reflects the finished task and a stopped orchestrator.
    let status = env.run(&["status", "--json"]);
    assert!(status.status.success());
    let doc: serde_json::Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(doc["orchestrator"]["running"], false);
    assert_eq!(doc["tasks"][0]["state"], "done");

    // `task show` renders the event history through terminal states.
    let show = env.run(&["task", "show", "1"]);
    assert!(show.status.success());
    assert!(stdout(&show).contains("done"));

    let _ = std::fs::remove_dir_all(&env.base);
}

#[test]
fn e2e_waiting_input_leaves_task_and_status_shows_it() {
    let env = setup(
        "waiting",
        "stream_states = [\"running\", \"waiting_input\"]\n",
        "none",
        "implement",
    );
    let out = env.run(&["run"]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains("waiting for input"),
        "summary flags the waiting task: {}",
        stdout(&out)
    );

    let status = env.run(&["status", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(doc["tasks"][0]["state"], "waiting_input");
    // The notifier received the waiting_input event (F-35/F-90).
    assert!(
        read_log(&env.notify_log)
            .iter()
            .any(|n| n["params"]["event"] == "waiting_input")
    );
    let _ = std::fs::remove_dir_all(&env.base);
}

#[test]
fn e2e_agent_crash_fails_task_and_orchestrator_survives() {
    let env = setup("crash", "crash_on_dispatch = true\n", "none", "implement");
    // The agent self-destructs on dispatch; the run must still exit cleanly
    // (crash isolation, §5.3), failing the affected task.
    let out = env.run(&["run"]);
    assert!(out.status.success(), "orchestrator survived the crash");
    assert!(
        stdout(&out).contains("failed 1"),
        "summary: {}",
        stdout(&out)
    );

    let status = env.run(&["status", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(doc["tasks"][0]["state"], "failed");
    let _ = std::fs::remove_dir_all(&env.base);
}

#[test]
fn e2e_dry_run_has_zero_side_effects() {
    let env = setup(
        "dry",
        "stream_states = [\"running\", \"done\"]\n",
        "source",
        "plan",
    );
    let out = env.run(&["run", "--dry-run"]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains("mock_src#1"),
        "dry-run lists the task"
    );

    // No task ingested: `task list --json` must be an empty array (the DB may
    // be created empty by opening it, which is acceptable).
    let listed = env.run(&["task", "list", "--json"]);
    let tasks: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap_or(json_empty());
    assert_eq!(tasks, serde_json::json!([]), "dry-run ingested no task");

    // No source write-back and no notification.
    assert!(read_log(&env.source_log).is_empty());
    assert!(read_log(&env.notify_log).is_empty());

    // No git/worktree side effects: no worktree materialized on disk, and the
    // bare origin has only `main` (no agent branch pushed).
    assert!(
        !env.state_dir().join("wt").exists(),
        "dry-run created no worktree"
    );
    let branch_output = test_support::git(
        &env.repo.join("origin.git"),
        &["branch", "--format=%(refname:short)"],
    );
    let branch_lines: Vec<&str> = branch_output.lines().collect();
    assert_eq!(
        branch_lines,
        ["main"],
        "dry-run pushed no branch: {branch_lines:?}"
    );
    let _ = std::fs::remove_dir_all(&env.base);
}

/// An empty JSON array (fallback when `task list --json` printed nothing).
fn json_empty() -> serde_json::Value {
    serde_json::json!([])
}
