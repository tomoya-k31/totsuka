# `totsukactl status` — 停止時出力の改善

日付: 2026-07-03
状態: 承認済み

## 背景 / 問題

supervisor 停止中に `totsukactl status` を実行すると:

```
stack not running          ← commands/status.rs が stdout に出力
Error: stack not running   ← main() の anyhow::Result が Debug 出力（二重）
Stack backtrace: ...       ← RUST_BACKTRACE=1 環境では backtrace まで出る
（exit code 1）
```

1. メッセージが二重に出て、backtrace ノイズが乗る。
2. 停止中でも取得できる情報（pgmq コンテナ、stale pid/sock）を一切出さないため、
   クラッシュ・orphan・残骸の診断ができない。
3. 「停止中」は異常ではないのに一律 exit 1 で、スクリプトから稼働判定できない。

## 設計

### 1. 停止時の出力

supervisor.sock 不達時、エラーではなく診断レポートを stdout に表示する:

```
SUPERVISOR  not running
pgmq        running
sock/       clean
pid files   none

hint: start the stack with `totsukactl up`
```

異常系の例:

```
SUPERVISOR  not running (stale supervisor.pid: pid 12345 is dead — crashed?)
pgmq        stopped
sock/       2 stale: qa.sock, adapter.sock
pid files   2 stale: orchestrator.pid (dead), agent-adapter.pid (pid 999 STILL ALIVE — orphan?)

hint: clean start with `totsukactl up`; orphan processes need manual kill
```

- **pgmq**: `ComposeExec::ps_running("pgmq")` で判定（`running` / `stopped`）。
  probe 失敗（docker デーモン停止等）は `unknown (docker unreachable)` に
  フォールバックし、status 自体は失敗させない。
- **sock/**: `paths.sock_dir` 内の残存ファイルを列挙。空なら `clean`。
- **pid files**: `supervisor.pid` + 子4つ（github-watcher / qa-service /
  orchestrator / agent-adapter）の pid ファイル残存を確認。
  残存 pid は `pidfile::process_alive` で生死判定し、
  生存していれば **orphan 警告**、死んでいれば `(dead)` を付ける。
- **hint 行**: 常に表示。orphan 検出時は手動 kill の注意を追記。
- 稼働時のテーブル出力は変更しない。

### 2. 終了コード規約（systemctl 互換風）

| 状況 | exit code |
|---|---|
| status: 稼働中（テーブル表示） | 0 |
| status: 停止中（診断レポート表示） | **3** |
| status: それ以外の失敗（config 不読等） | 1 |
| 他コマンド: 成功 / 失敗 | 0 / 1（従来どおり） |

- `cli::dispatch` の戻り値を `Result<(), TotsukactlError>` →
  `Result<std::process::ExitCode, TotsukactlError>` に変更。
  Status 以外のアームは成功時 `ExitCode::SUCCESS`。
- `main()` は `Ok(code) → code` / `Err(e) → eprintln!("error: {e}") + exit 1`。
  anyhow の Debug 出力（backtrace 含む）を廃止 — 全コマンドのエラーが
  簡潔な1行になる。
- `down` の `NotRunning` は従来どおりエラー（exit 1）。実バグのシグナルとして扱う
  smoke-test の前提を維持する。

### 3. 実装構造

- `commands/status.rs`:
  - `NotRunningReport { pgmq: PgmqProbe, stale_socks: Vec<String>, stale_pids: Vec<StalePid>, supervisor_pid: Option<StalePid> }`
  - 収集 `gather_not_running_report(paths, compose) -> NotRunningReport`
  - 描画 `format_not_running(&report) -> String`（純関数・ユニットテスト対象)
  - `run(...)` は `Result<std::process::ExitCode, TotsukactlError>` を返す
- `cli.rs` Status アーム: `DockerCompose::new(cfg.postgres.compose_file)` を
  渡す（`init.rs` と同じイディオム）。

### 4. テスト

1. `format_not_running` 純関数テスト: clean / stale sock / dead pid /
   orphan pid / pgmq unknown の各表示。
2. `run()` 統合テスト（tempdir Paths + MockCompose）: sock 不達 →
   `ExitCode(3)`、レポートに期待行が含まれる。stale pid 配置 → stale 表示。
3. 既存 `status_format.rs`（稼働時テーブル）は無変更で green を維持。

## スコープ外

- 稼働時テーブルの変更
- `--json` 出力
- orphan の自動 kill
