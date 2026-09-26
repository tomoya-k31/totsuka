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
//! one-shot runs. The one exception is the stop-signal tests (#753): only a
//! `--watch` run is still running when the signal lands. They wait for
//! `health.json` (written by every cycle) instead of sleeping, and every wait
//! is capped.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use test_support::{plugin_section, scratch};

/// Path to the compiled `totsuka` binary.
fn totsuka() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_totsuka"))
}

/// Path to the `mock_plugin` binary (a bin of `orchestrator-core`, so
/// `CARGO_BIN_EXE_*` does not cover it). Built once per test process, or not at
/// all when CI has pre-built the workspace (#281).
fn mock_plugin() -> PathBuf {
    test_support::sibling_bin(&totsuka(), "orchestrator-core", "mock_plugin")
}

/// The XDG-scoped environment for a scratch base.
struct Env {
    base: PathBuf,
    repo: PathBuf,
    source_log: PathBuf,
    notify_log: PathBuf,
}

/// One-shot's quiet-period floor for the E2Es (#281). Production is 2s; these
/// runs drive a mock source whose `task/submit` lands in the first cycle, so
/// 250ms is still a real cushion — and four `run` invocations stop costing 8s
/// of pure waiting.
///
/// Deliberately not 0: the grace exists because `task/submit` arrives
/// asynchronously from a freshly spawned plugin subprocess, and 0 would race
/// the handshake and flake on a loaded runner.
const GRACE: &[&str] = &["--one-shot-grace-ms", "250"];

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
        self.wait(self.spawn(args), args)
    }

    /// Start `totsuka <args>` with XDG pointed at the scratch dirs, without
    /// waiting for it.
    fn spawn(&self, args: &[&str]) -> Child {
        self.command(args).spawn().unwrap()
    }

    /// `totsuka <args>` with `line` written to stdin, which is then **kept
    /// open** until the child exits — the way a launcher holds the pipe
    /// (#754). A child that waited for EOF would hang into the 60s guard.
    fn run_with_stdin(&self, args: &[&str], line: &str) -> Output {
        let mut child = self.command(args).stdin(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        // The child may exit (a rejected line) before reading everything.
        let _ = stdin.write_all(line.as_bytes());
        let out = self.wait(child, args);
        drop(stdin);
        out
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(totsuka());
        command
            .args(args)
            .env("XDG_CONFIG_HOME", self.base.join("cfg"))
            .env("XDG_DATA_HOME", self.base.join("data"))
            .env("XDG_STATE_HOME", self.base.join("state"))
            .env("XDG_CACHE_HOME", self.base.join("cache"))
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Collect a [`spawn`](Self::spawn)ed child under the 60s wall-clock guard.
    fn wait(&self, mut child: Child, args: &[&str]) -> Output {
        let start = Instant::now();
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
    test_support::place_binary(&mock_plugin(), &dir.join(name));
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "name = \"{name}\"\nkind = \"{kind}\"\nversion = \"0.1.0\"\n\
             protocol_version = \">=0.6.0, <0.8\"\n\n[capabilities]\nstate_stream = true\n\
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
    std::fs::create_dir_all(&cfg_dir).unwrap();
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
location = "{state}/wt/{{repo_name}}/{{worktree_name}}"
cleanup = "immediate"
plan_cleanup = "immediate"

[[projects]]
name = "mock_src"
source = "mock_src"

[[workflows]]
name = "wf"
projects = ["mock_src"]
trigger = {{}}
mode = "{mode}"
agent = "mock_agent"
output = "{output}"
on_success = {{ status = "レビュー待ち" }}

{mock_src}
{mock_agent}
{mock_notify}
"#,
            clone = env.repo.join("clone").display(),
            state = env.state_dir().display(),
            mock_src = plugin_section(
                "mock_src",
                &format!(
                    "notify_log = \"{}\"\ntask_submit = true\n[[submit_tasks]]\nid = \"1\"\nsource = \"mock_src\"\ntitle = \"e2e task\"\n",
                    env.source_log.display()
                ),
            ),
            mock_agent = plugin_section("mock_agent", agent_cfg),
            mock_notify = plugin_section(
                "mock_notify",
                &format!("notify_log = \"{}\"\n", env.notify_log.display()),
            ),
        ),
    )
    .unwrap();

    env
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Read a recorded NDJSON log (empty if never written).
fn read_log(path: &std::path::Path) -> Vec<serde_json::Value> {
    test_support::read_ndjson_log(path)
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
    let out = env.run(&[&["run"], GRACE].concat());
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

/// `totsuka run --json` puts the whole `RunSummary` on stdout and nothing else
/// (#462), so a caller can act on the run instead of grepping prose.
#[test]
fn e2e_run_json_emits_only_the_summary_document() {
    let env = setup(
        "runjson",
        "stream_states = [\"running\", \"done\"]\n",
        "source",
        "plan",
    );

    let out = env.run(&[&["run", "--json"], GRACE].concat());
    assert!(out.status.success(), "run failed: {}", stdout(&out));

    // Parsing *the whole of stdout* is the assertion that matters: the
    // `--json` contract is "nothing but the document", so a stray prose line
    // has to fail here rather than merely add noise a `contains` would miss.
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["stats"]["done"], 1, "document: {doc}");
    assert_eq!(doc["stats"]["failed"], 0, "document: {doc}");
    assert_eq!(doc["interrupted"], false, "document: {doc}");
    assert!(
        doc["waiting"].as_array().is_some_and(|a| a.is_empty()),
        "waiting is an empty array, not absent: {doc}"
    );

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
    let out = env.run(&[&["run"], GRACE].concat());
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
fn e2e_agent_crash_is_isolated_and_the_orchestrator_survives() {
    let env = setup("crash", "crash_on_dispatch = true\n", "none", "implement");
    // The agent self-destructs on dispatch. What this level can state
    // deterministically is crash *isolation* (§5.3): the run exits cleanly and
    // does not lose the task. What it cannot state is any count or terminal
    // state — see below for both.
    //
    // **The task's terminal state is deliberately not asserted.** Since #504 it
    // depends on which of two correct paths wins: the dispatch reaching the
    // dying plugin (transport error -> `failed`), or the supervisor marking the
    // plugin down first (-> parked, left `queued`). This test's `GRACE` is
    // 250 ms while the first restart backoff is one second, so no restart can
    // land inside the window either way — asserting either outcome makes the
    // test race the machine it runs on. It did: asserting `failed` passed
    // locally and failed on CI.
    //
    // Those state-machine contracts have their own coverage in
    // orchestrator-core's `plugin_supervision.rs`, which drives the backoff
    // explicitly instead of racing wall-clock time — see
    // `a_task_queued_during_a_crash_window_is_not_failed` and
    // `giving_up_escalates_instead_of_retrying_forever`.
    let out = env.run(&[&["run", "--json"], GRACE].concat());
    assert!(out.status.success(), "orchestrator survived the crash");

    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    // **The crash count is deliberately not asserted here.** Two observers
    // write it: the dispatch call site records the transport error on the
    // method, and `on_plugin_closed` — driven by the child's own exit —
    // increments `plugin_crashes`. Only the second can still be behind when
    // this one-shot run ends, so asserting it races the machine. #512 caught
    // exactly that, on a branch with no Rust changes at all.
    //
    // The contract itself is pinned deterministically in orchestrator-core's
    // `plugin_supervision.rs`, which waits for the condition instead of a
    // wall clock — see `a_task_queued_during_a_crash_window_is_not_failed`
    // and `giving_up_escalates_instead_of_retrying_forever`.
    //
    // What this level *can* state is that the crash was isolated: the run
    // ended cleanly rather than being torn down.
    assert_eq!(doc["interrupted"], false, "document: {doc}");

    // Whichever path won, the task is still there to retry.
    let status = env.run(&["status", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(
        doc["tasks"].as_array().map(Vec::len),
        Some(1),
        "the crash must not lose the task: {doc}"
    );
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
    // Every source is push-only (0.2.0): nothing is fetched ahead of time,
    // so `--dry-run` has no preview to show.
    assert!(
        stdout(&out).contains("cannot be previewed"),
        "dry-run reports no preview available: {}",
        stdout(&out)
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

/// `doctor` の孤児 pane 検出（#211、protocol 0.2.2 `session/list`）。
/// mock agent が pane 一覧を返し、doctor が DB と突き合わせて「終端タスクかつ
/// worktree 消滅の pane と DB 未知の pane を候補にし、非終端タスクの pane は
/// 候補にしない」ことを、非 TTY（`--json`）の検出のみ経路で固定する。
/// A workflow key nobody claims stops the run (#554).
///
/// This is what replaced `deny_unknown_fields` on `WorkflowConfig`: the
/// Orchestrator cannot tell a typo from a plugin's option, so it asks, and a
/// key with no owner is refused rather than carried along doing nothing.
#[test]
fn an_unclaimed_workflow_key_refuses_to_run() {
    let env = setup(
        "unclaimed-option",
        "stream_states = [\"running\", \"done\"]\n",
        "none",
        "plan",
    );
    let config = env.cfg_dir().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    // `profil` is what a mistyped `profile` looks like. Nothing claims it.
    std::fs::write(
        &config,
        text.replace(
            "\nagent = \"mock_agent\"\n",
            "\nagent = \"mock_agent\"\nprofil = \"triage\"\n",
        ),
    )
    .unwrap();

    let out = env.run(&[&["run"], GRACE].concat());
    // A config error: exit 4, so a supervisor does not restart into it (#755).
    assert_eq!(out.status.code(), Some(4), "{}", stdout(&out));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("profil"), "{err}");
    assert!(
        err.contains("mock_src") && err.contains("mock_agent"),
        "the message must name who was asked: {err}"
    );
}

/// …and the same key runs once a plugin says it is its own. The pair matters:
/// without this half, a `check_workflow_options` that rejected *everything*
/// would pass the test above.
#[test]
fn a_claimed_workflow_key_runs() {
    let env = setup(
        "claimed-option",
        "stream_states = [\"running\", \"done\"]\n",
        "none",
        "plan",
    );
    let config = env.cfg_dir().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        text.replace(
            "\nagent = \"mock_agent\"\n",
            "\nagent = \"mock_agent\"\nthread_scope = \"parent\"\n",
        )
        .replace(
            "[mock_src]\n",
            "[mock_src]\nclaim_options = [\"thread_scope\"]\n",
        ),
    )
    .unwrap();

    let out = env.run(&[&["run"], GRACE].concat());
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        stdout(&out),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn doctor_detects_orphan_panes_via_session_list() {
    use orchestrator_core::adapters::{NewTask, StateDb};
    use orchestrator_core::domain::state::TaskEvent;

    let base = scratch("orphan-panes");
    let env = Env {
        source_log: base.join("source.ndjson"),
        notify_log: base.join("notify.ndjson"),
        base,
        repo: PathBuf::new(),
    };
    let cfg_dir = env.cfg_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::create_dir_all(env.state_dir()).unwrap();

    // pane_control 宣言つき agent_ide として mock を install（既定の
    // install_plugin は pane_control を宣言しないため手書き）。
    let dir = env.plugins_store().join("mock_agent");
    std::fs::create_dir_all(&dir).unwrap();
    test_support::place_binary(&mock_plugin(), &dir.join("mock_agent"));
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"mock_agent\"\nkind = \"agent_ide\"\nversion = \"0.1.0\"\n\
         protocol_version = \">=0.6.0, <0.8\"\n\n[capabilities]\nstate_stream = true\n\
         pane_control = true\n",
    )
    .unwrap();

    // mock の `session/list` 応答は `[mock_agent]` で staging する（#554）。
    std::fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "[plugins.mock_agent]\nenabled = true\nkind = \"agent_ide\"\n\n{}",
            plugin_section(
                "mock_agent",
                r#"list_sessions = [
  { session_id = "w1:p1|", label = "totsuka C9:9.9" },
  { session_id = "w2:p1|", label = "totsuka C1:1.0" },
  { session_id = "w3:p1|", label = "totsuka 99" },
]
"#,
            )
        ),
    )
    .unwrap();

    // DB: task 1 = cancelled（終端）で worktree 記録なし → 候補。
    //     task 2 = running（非終端）→ 候補にしない。
    let db = StateDb::open(&env.state_dir().join("state.db")).unwrap();
    let new = |sid: &str| NewTask {
        source: "mock_src".into(),
        source_task_id: sid.into(),
        workflow: "wf".into(),
        mode: "implement".into(),
        repo: None,
        priority: 0,
        title: format!("task {sid}"),
        url: None,
        source_payload: None,
        last_signal_at: None,
    };
    let cancelled = db.upsert_task(&new("C9:9.9")).unwrap();
    db.apply_event(db.task_ref(cancelled).unwrap(), TaskEvent::Cancel, None)
        .unwrap();
    let running = db.upsert_task(&new("C1:1.0")).unwrap();
    db.apply_event(db.task_ref(running).unwrap(), TaskEvent::Dispatch, None)
        .unwrap();
    db.apply_event(db.task_ref(running).unwrap(), TaskEvent::Start, None)
        .unwrap();
    drop(db);

    let out = env.run(&["doctor", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "orphan panes are found-problems (exit 3): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("doctor --json parses");
    let panes = doc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "panes")
        .expect("panes check present");
    assert_eq!(panes["ok"], false, "{panes}");
    let detail = panes["detail"].as_str().unwrap();
    assert!(
        detail.contains("totsuka C9:9.9"),
        "terminal+gone-worktree pane listed: {detail}"
    );
    assert!(
        detail.contains("totsuka 99"),
        "DB-unknown pane listed: {detail}"
    );
    assert!(
        !detail.contains("totsuka C1:1.0"),
        "running task's pane must be kept: {detail}"
    );
    assert!(
        panes["action"].as_str().unwrap().contains("terminal"),
        "action points at the interactive path: {panes}"
    );
}

/// `doctor` の human 出力の無害化（#297）。pane label は
/// `totsuka {source_task_id}`（ADR-0013）＝**外部が内容を決める id** を含むので、
/// `task show` / `status` と同じ攻撃がそのまま通る。しかも `doctor` は
/// 「何かが既におかしい」ときにこそ読まれる。
///
/// human 出力に生の `ESC` / `CR` が無いこと・ペイロードが消えていないこと・
/// panes の行が 1 行のままであること、そして `--json` の値が **投稿された
/// ものとバイト単位で一致する**（二重エスケープしない）ことを固定する。
#[test]
fn doctor_human_output_cannot_repaint_the_terminal_yet_json_stays_verbatim() {
    use orchestrator_core::adapters::StateDb;

    let base = scratch("doctor-control-sequences");
    let env = Env {
        source_log: base.join("source.ndjson"),
        notify_log: base.join("notify.ndjson"),
        base,
        repo: PathBuf::new(),
    };
    let cfg_dir = env.cfg_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::create_dir_all(env.state_dir()).unwrap();

    let dir = env.plugins_store().join("mock_agent");
    std::fs::create_dir_all(&dir).unwrap();
    test_support::place_binary(&mock_plugin(), &dir.join("mock_agent"));
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"mock_agent\"\nkind = \"agent_ide\"\nversion = \"0.1.0\"\n\
         protocol_version = \">=0.6.0, <0.8\"\n\n[capabilities]\nstate_stream = true\n\
         pane_control = true\n",
    )
    .unwrap();
    // ESC[2J clears the screen, ESC[1A walks the cursor back over the row
    // already printed, and the bare CR rewrites the current row from column 0
    // — the pane listing is the last place an operator should be reading a
    // forged screen, since the next thing they do is release panes.
    let esc = char::from_u32(0x1b).unwrap();
    let label = format!("totsuka C9:{esc}[2Jinnocent{esc}[1A\rforged");
    std::fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "[plugins.mock_agent]\nenabled = true\nkind = \"agent_ide\"\n\n{}",
            plugin_section(
                "mock_agent",
                // Written with TOML's own escapes so the staged text itself
                // stays printable; the plugin reports the decoded bytes.
                "list_sessions = [\n  { session_id = \"w1:p1|\", \
                 label = \"totsuka C9:\\u001B[2Jinnocent\\u001B[1A\\rforged\" },\n]\n",
            )
        ),
    )
    .unwrap();

    // An empty DB is enough: the label matches no task, which is the plain
    // "true orphan" case.
    StateDb::open(&env.state_dir().join("state.db")).unwrap();

    let out = env.run(&["doctor"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "orphan panes are found-problems (exit 3): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(!text.contains(esc), "doctor emitted a live ESC: {text:?}");
    assert!(!text.contains('\r'), "doctor emitted a bare CR: {text:?}");
    // Neutralised, not deleted: what the pane actually carries is readable.
    assert!(
        text.contains("innocent") && text.contains("forged"),
        "doctor swallowed the payload text: {text}"
    );
    // A check is a line: an escape must not be able to invent or erase rows.
    assert_eq!(
        text.lines().filter(|l| l.contains("panes")).count(),
        1,
        "the panes check split rows: {text:?}"
    );

    // --json keeps the bytes the pane reported, escaped once by serde_json.
    let out = env.run(&["doctor", "--json"]);
    let raw = stdout(&out);
    assert!(!raw.contains(esc), "raw JSON carried a live ESC: {raw:?}");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("doctor --json parses");
    let panes = doc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "panes")
        .expect("panes check present");
    assert!(
        panes["detail"].as_str().unwrap().contains(&label),
        "--json must carry the label verbatim, not the escaped form: {panes}"
    );
}

/// `run --watch` stops gracefully on every stop request, not just Ctrl-C
/// (#753): launchd / `brew services` / `kill` send SIGTERM and a closed
/// terminal sends SIGHUP. Their default action kills the process before
/// `engine.shutdown`, leaving `health.json` behind — the file `menu` reads.
fn stops_gracefully_on(signal: &str) {
    let env = setup(
        &format!("stop_{signal}"),
        "stream_states = [\"running\", \"done\"]\n",
        "source",
        "plan",
    );
    let health = env.state_dir().join("health.json");
    let lock = env.state_dir().join("run.lock");
    // Not under the scratch dir: macOS caps a socket path at 104 bytes, and
    // `$TMPDIR` there is already most of that.
    let socket = PathBuf::from(format!("/tmp/totsuka-{}-{signal}.sock", std::process::id()));
    let config = env.cfg_dir().join("config.toml");
    let mut toml = std::fs::read_to_string(&config).unwrap();
    toml.push_str(&format!(
        "\n[hooks]\nsocket_path = \"{}\"\n",
        socket.display()
    ));
    std::fs::write(&config, toml).unwrap();

    let args = ["run", "--watch", "--json"];
    let mut child = env.spawn(&args);
    // Readiness: the first cycle writes health.json, and it runs
    // after the socket is bound and the stop listeners are installed.
    let start = Instant::now();
    while !health.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("run exited before its first cycle: {status}");
        }
        if start.elapsed() >= Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("run never wrote {}", health.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Every failure before `env.wait` kills the child first: a `--watch` run
    // never exits on its own, and a leaked one keeps holding its lock.
    let sent = lock.exists()
        && socket.exists()
        && Command::new("kill")
            .args([format!("-{signal}"), child.id().to_string()])
            .status()
            .is_ok_and(|s| s.success());
    if !sent {
        let _ = child.kill();
        let _ = child.wait();
        panic!("run was not holding its lock and socket, or kill -{signal} failed");
    }
    let out = env.wait(child, &args);

    assert!(
        out.status.success(),
        "SIG{signal} must stop the run gracefully, got {} (stderr: {})",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["interrupted"], true, "document: {doc}");
    assert!(!health.exists(), "health.json must be cleared");
    assert!(!lock.exists(), "run.lock must be released");
    assert!(!socket.exists(), "the hook socket must be unlinked");

    let _ = std::fs::remove_dir_all(&env.base);
}

#[test]
fn run_watch_stops_gracefully_on_sigterm() {
    stops_gracefully_on("TERM");
}

#[test]
fn run_watch_stops_gracefully_on_sighup() {
    stops_gracefully_on("HUP");
}

#[test]
fn run_watch_stops_gracefully_on_sigint() {
    stops_gracefully_on("INT");
}

// ---------------------------------------------------------------------------
// `run`'s startup exit codes (#755): 4 = only a person can fix it, 5 = another
// orchestrator holds the lock, 1 = everything else. A supervisor restarts on 1
// and stops on 4 / 5, so a misclassification either loops forever or gives up
// on something a restart would have fixed.
// ---------------------------------------------------------------------------

/// Run `run` and require exit `code`; returns stderr for further checks.
fn run_expecting(env: &Env, code: i32) -> String {
    let out = env.run(&[&["run"], GRACE].concat());
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(code), "stderr: {err}");
    err
}

#[test]
fn run_exits_4_on_a_config_that_does_not_parse() {
    let env = setup("exit4-parse", "", "none", "plan");
    std::fs::write(env.cfg_dir().join("config.toml"), "[plugins.mock_src\n").unwrap();

    // `--json` keeps the existing envelope; the code is the only new signal.
    let out = env.run(&[&["run", "--json"], GRACE].concat());
    assert_eq!(out.status.code(), Some(4));
    let envelope: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stderr).trim())
            .expect("stderr is the JSON error envelope");
    assert!(envelope["error"]["message"].is_string(), "{envelope}");
    assert!(stdout(&out).is_empty(), "stdout: {}", stdout(&out));
}

#[test]
fn run_exits_4_when_an_env_file_is_missing() {
    let env = setup("exit4-env-file", "", "none", "plan");
    let config = env.cfg_dir().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let tools = "[tools.claude]\nkind = \"claude\"\nenv_file = \"/nonexistent/totsuka-755.env\"\n";
    std::fs::write(&config, format!("{text}\n{tools}")).unwrap();
    let err = run_expecting(&env, 4);
    assert!(err.contains("env_file"), "{err}");
}

#[test]
fn run_exits_4_when_a_plugin_secret_does_not_resolve() {
    let env = setup(
        "exit4-secret",
        "stream_states = [\"running\", \"done\"]\n",
        "none",
        "plan",
    );
    let config = env.cfg_dir().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    // `cmd:false` always exits non-zero — a `SecretError::Backend`, the kind
    // a locked vault produces, without depending on a real Keychain or `op`.
    std::fs::write(
        &config,
        text.replace("[mock_src]\n", "[mock_src]\ntoken = \"cmd:false\"\n"),
    )
    .unwrap();
    let err = run_expecting(&env, 4);
    assert!(err.contains("mock_src"), "{err}");
}

#[test]
fn run_exits_4_when_a_plugin_rejects_its_config() {
    let env = setup("exit4-reject", "reject_config = true\n", "none", "plan");
    let err = run_expecting(&env, 4);
    assert!(err.contains("mock rejected its config"), "{err}");
}

#[test]
fn run_exits_4_when_a_plugin_binary_is_missing() {
    let env = setup("exit4-no-binary", "", "none", "plan");
    std::fs::remove_file(env.plugins_store().join("mock_agent/mock_agent")).unwrap();
    let err = run_expecting(&env, 4);
    assert!(err.contains("mock_agent"), "{err}");
}

#[test]
fn run_exits_5_while_another_orchestrator_holds_the_lock() {
    let env = setup("exit5-lock", "", "none", "plan");
    // This test process is alive, so its pid is a live holder.
    std::fs::write(
        env.state_dir().join("run.lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    let err = run_expecting(&env, 5);
    assert!(err.contains("already running"), "{err}");
}

/// The control: a failure a restart may fix stays the generic 1. Without this,
/// mapping every startup error to 4 would pass the tests above.
#[test]
fn run_still_exits_1_when_the_state_db_cannot_open() {
    let env = setup("exit1-db", "", "none", "plan");
    // A directory where the DB file should be: an IO failure, not config.
    std::fs::create_dir_all(env.state_dir().join("state.db")).unwrap();
    run_expecting(&env, 1);
}

#[test]
fn run_exits_4_when_a_plugin_speaks_an_incompatible_protocol() {
    let env = setup("exit4-protocol", "", "none", "plan");
    let manifest = env.plugins_store().join("mock_notify/plugin.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace(">=0.6.0, <0.8", ">=99.0.0")).unwrap();
    let err = run_expecting(&env, 4);
    assert!(err.contains("protocol-incompatible"), "{err}");
}

// ---------------------------------------------------------------------------
// `run --secrets-stdin` (#754): a launcher hands over every value on stdin's
// first line and config names them `secret:<name>`. The run must deliver them
// to plugins and agents, and must never reach a secret store.
// ---------------------------------------------------------------------------

/// Point `mock_src`'s token and a tool's `env_file` at `secret:` names, and
/// record what the plugins receive.
fn setup_supplied(name: &str) -> (Env, PathBuf, PathBuf) {
    let base = scratch(&format!("{name}-logs"));
    let init_log = base.join("init.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");
    let env = setup(
        name,
        &format!(
            "stream_states = [\"running\", \"done\"]\ndispatch_log = \"{}\"\n",
            dispatch_log.display()
        ),
        "none",
        "plan",
    );
    let env_file = env.base.join("agent.env");
    std::fs::write(&env_file, "AGENT_KEY=secret:agent-key\n").unwrap();
    let config = env.cfg_dir().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replace(
        "[mock_src]\n",
        &format!(
            "[mock_src]\ntoken = \"secret:src-token\"\ninit_log = \"{}\"\n",
            init_log.display()
        ),
    );
    std::fs::write(
        &config,
        format!(
            "default_tool = \"t\"\n{text}\n[tools.t]\nkind = \"claude\"\nenv_file = \"{}\"\n",
            env_file.display()
        ),
    )
    .unwrap();
    (env, init_log, dispatch_log)
}

#[test]
fn run_with_secrets_stdin_delivers_supplied_values_to_plugins_and_agents() {
    let (env, init_log, dispatch_log) = setup_supplied("supplied-ok");
    // A second line and an unused name: neither may matter.
    let out = env.run_with_stdin(
        &[&["run", "--secrets-stdin"], GRACE].concat(),
        "{\"src-token\":\"tok-123\",\"agent-key\":\"key-456\",\"unused\":\"x\"}\n{\"later\":\"line\"}\n",
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let init = read_log(&init_log);
    let token = init
        .iter()
        .find_map(|e| e["params"]["config"]["token"].as_str())
        .expect("mock_src initialize recorded");
    assert_eq!(token, "tok-123");

    let dispatch = read_log(&dispatch_log);
    let env_map = &dispatch
        .iter()
        .find(|e| e["method"] == "task/dispatch")
        .expect("a dispatch")["params"]["tool_launch"]["env"];
    assert_eq!(
        env_map["AGENT_KEY"], "key-456",
        "tool_launch.env: {env_map}"
    );

    let _ = std::fs::remove_dir_all(&env.base);
}

#[test]
fn a_secret_reference_without_secrets_stdin_exits_4_and_says_how() {
    let (env, ..) = setup_supplied("supplied-missing-flag");
    let err = run_expecting(&env, 4);
    assert!(err.contains("--secrets-stdin"), "{err}");
}

/// The strict half: under `--secrets-stdin` a store-backed reference is an
/// error, and its backend is never reached. The `cmd:` would create `marker`
/// if it ran — the file's absence is the proof, not the message.
#[test]
fn secrets_stdin_refuses_a_store_reference_without_running_it() {
    let (env, ..) = setup_supplied("supplied-strict");
    let marker = env.base.join("cmd-ran");
    let config = env.cfg_dir().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replace(
        "token = \"secret:src-token\"",
        &format!("token = \"cmd:touch {} && echo x\"", marker.display()),
    );
    std::fs::write(&config, text).unwrap();

    let out = env.run_with_stdin(
        &[&["run", "--secrets-stdin"], GRACE].concat(),
        "{\"agent-key\":\"key-456\"}\n",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(4), "stderr: {err}");
    assert!(err.contains("secret:<name>"), "{err}");
    assert!(
        !marker.exists(),
        "the cmd: backend ran under --secrets-stdin"
    );
}

/// A dry run reads (and checks) the line too, so a launcher's write neither
/// blocks nor hits EPIPE. It still launches the plugins — which resolve their
/// tables from the line like a real run — and skips only `env_file`
/// (ADR-0090), so `agent-key` may be absent here.
#[test]
fn a_dry_run_reads_and_checks_the_line() {
    let (env, ..) = setup_supplied("supplied-dry-run");
    let bad = env.run_with_stdin(
        &["run", "--dry-run", "--secrets-stdin"],
        "{\"src-token\": 42}\n",
    );
    let err = String::from_utf8_lossy(&bad.stderr);
    assert_eq!(bad.status.code(), Some(4), "stderr: {err}");
    assert!(err.contains("`src-token` is not a string"), "{err}");
    assert!(!err.contains("42"), "{err}");

    let ok = env.run_with_stdin(
        &["run", "--dry-run", "--secrets-stdin"],
        "{\"src-token\":\"tok-123\"}\n",
    );
    assert!(
        ok.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
}
