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
             protocol_version = \">=0.1.6, <0.4\"\n\n[capabilities]\nstate_stream = true\n\
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
location = "{state}/wt/{{repo_name}}/{{worktree_name}}"
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
            "notify_log = \"{}\"\ntask_submit = true\n[[submit_tasks]]\nid = \"1\"\nsource = \"mock_src\"\ntitle = \"e2e task\"\n",
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
fn e2e_agent_crash_fails_task_and_orchestrator_survives() {
    let env = setup("crash", "crash_on_dispatch = true\n", "none", "implement");
    // The agent self-destructs on dispatch; the run must still exit cleanly
    // (crash isolation, §5.3), failing the affected task.
    let out = env.run(&[&["run"], GRACE].concat());
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
    std::fs::create_dir_all(cfg_dir.join("plugins")).unwrap();
    std::fs::create_dir_all(env.state_dir()).unwrap();

    // pane_control 宣言つき agent_ide として mock を install（既定の
    // install_plugin は pane_control を宣言しないため手書き）。
    let dir = env.plugins_store().join("mock_agent");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(mock_plugin(), dir.join("mock_agent")).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"mock_agent\"\nkind = \"agent_ide\"\nversion = \"0.1.0\"\n\
         protocol_version = \">=0.1.6, <0.4\"\n\n[capabilities]\nstate_stream = true\n\
         pane_control = true\n",
    )
    .unwrap();

    std::fs::write(
        cfg_dir.join("config.toml"),
        "[plugins.mock_agent]\nenabled = true\nkind = \"agent_ide\"\n",
    )
    .unwrap();
    // mock の `session/list` 応答を plugins/{name}.toml で staging する。
    std::fs::write(
        cfg_dir.join("plugins/mock_agent.toml"),
        r#"list_sessions = [
  { session_id = "w1:p1|", label = "totsuka C9:9.9" },
  { session_id = "w2:p1|", label = "totsuka C1:1.0" },
  { session_id = "w3:p1|", label = "totsuka 99" },
]
"#,
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
    db.apply_event(cancelled, TaskEvent::Cancel, None).unwrap();
    let running = db.upsert_task(&new("C1:1.0")).unwrap();
    db.apply_event(running, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(running, TaskEvent::Start, None).unwrap();
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
    std::fs::create_dir_all(cfg_dir.join("plugins")).unwrap();
    std::fs::create_dir_all(env.state_dir()).unwrap();

    let dir = env.plugins_store().join("mock_agent");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(mock_plugin(), dir.join("mock_agent")).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"mock_agent\"\nkind = \"agent_ide\"\nversion = \"0.1.0\"\n\
         protocol_version = \">=0.1.6, <0.4\"\n\n[capabilities]\nstate_stream = true\n\
         pane_control = true\n",
    )
    .unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[plugins.mock_agent]\nenabled = true\nkind = \"agent_ide\"\n",
    )
    .unwrap();

    // ESC[2J clears the screen, ESC[1A walks the cursor back over the row
    // already printed, and the bare CR rewrites the current row from column 0
    // — the pane listing is the last place an operator should be reading a
    // forged screen, since the next thing they do is release panes.
    let esc = char::from_u32(0x1b).unwrap();
    let label = format!("totsuka C9:{esc}[2Jinnocent{esc}[1A\rforged");
    std::fs::write(
        cfg_dir.join("plugins/mock_agent.toml"),
        // Written with TOML's own escapes so the staging file itself
        // stays printable; the plugin reports the decoded bytes.
        "list_sessions = [\n  { session_id = \"w1:p1|\", \
         label = \"totsuka C9:\\u001B[2Jinnocent\\u001B[1A\\rforged\" },\n]\n",
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
