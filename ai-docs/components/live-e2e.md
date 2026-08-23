---
type: Tool
title: live-e2e スキル
description: 実 Slack / 実 GitHub / 実 herdr + 実 Claude Code に対して totsuka を通しで動かす実機検証の手順・設定雛形・駆動スクリプト一式。自動／手動／目視の区分と、別環境での一からの構築手順を含む。
resource: https://github.com/tomoya-k31/totsuka/tree/main/.claude/skills/live-e2e
tags: [testing, e2e, skill, tooling, slack, github, herdr]
generated: { by: claude-code/opus-5, at: 2026-08-23T00:00:00Z }
status: stable
owner: tomoya-k31
---

# 責務

[テスト戦略](/quality/test-strategy.md) が定める「自動化対象外」の領域 — 実機エージェント・
実 Slack・実 GitHub との接続 — を、手順として実行可能にする。CI（`slack_e2e.rs` 等）は
モックに対して全経路を通すので、**実接続でしか出ない不具合はここでしか捕まらない**。

2026-08-03 の初回実機検証では、この経路でプロダクトの不具合が 4 種見つかった
（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) の protocol 17 非互換、pane と
エージェントの起動過渡状態 2 種、[#382](https://github.com/tomoya-k31/totsuka/issues/382) の
D-03 アンカー）。いずれも実機で走らせるまで検出されていない。

# 構成

| パス | 内容 |
|---|---|
| `SKILL.md` | 実行の流れ。自動／手動／目視の区分と、代行できない操作の一覧 |
| `references/bootstrap.md` | 別環境で一から作る手順（アカウント・Slack アプリ 3 つ・トークン 5 本・サンドボックス repo・ProjectsV2） |
| `references/scenarios.md` | 全テストパターンと各々の検証点。実施順も定義する |
| `references/troubleshooting.md` | 症状から原因を引く表。実際に踏んだ失敗モード |
| `assets/env.sample` | `.env` の雛形。全環境変数と `tt()` ラッパー |
| `assets/cfg/` | `config.toml` / `plugins/{slack,github,herdr}.toml` / mock agent の `plugin.toml` |
| `scripts/bootstrap.sh` | `$E2E_HOME` 構築とプラグイン install（既存設定は上書きしない） |
| `scripts/bootstrap-github.sh` | サンドボックス repo 2 つ・Project・seed Issue（冪等） |
| `scripts/slack.sh` | Slack の駆動と観測。**投稿コマンドは意図的に持たない**（下記） |
| `scripts/github.sh` | Project の操作と F-07 / F-84 / F-86 の自動判定 |
| `scripts/report.sh` | 結果の集約。目視項目とスコープ外を明示して残す |
| `scripts/github-permissions.sh` | GitHub トークンの権限を実測する単発プローブ。プラグインが投げる 4 操作（read 3 + write 1）を同じ endpoint / header / クエリで送り、`errors` の有無と**独立に**フィールド単位で present/null を判定する — GraphQL の権限不足は HTTP 200 + `data` あり + フィールド `null` で出うるため。`doctor --online` が `viewer` 1 操作しか叩かない（F-59）ことで生じる隙間を埋める |

# 設計上の判断

**環境は XDG で隔離する。** `--config` は `config.toml` しか差し替えず、状態 DB もプラグイン
ストアも本番側を触るため使えない。ただし `XDG_CONFIG_HOME` を `export` すると `gh` が
`$XDG_CONFIG_HOME/gh` を読んで認証が壊れるので、`tt()` が `env` で totsuka の起動時にだけ被せる。

**`slack.sh` に投稿コマンドが無いのは意図的である。** `chat.postMessage` は user token で
投稿しても `bot_id` が付き、[task-source-slack](/components/task-source-slack.md) の
メンション判定表①が必ず除外する。メンションもリアクションの対象メッセージも、**人間が
クライアントで打つ以外に作れない**。持たせると「なぜか起動しない」を再生産する。

**承認ボタンの押下は自動化できない。** Slack に block_actions を発火させる API は無く、
Socket Mode は Slack → アプリの一方向。合成するには `api_url` をプロキシへ向けて envelope を
注入する必要があり、それは実 Slack の検証ではなくなる。

**`op://` を使うと常駐プロセスは人間のターミナルからしか起動できない。** `op read` は
デスクトップ承認を要求し、非対話シェルでは `authorization timeout` になる
（[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md) の前提どおりの挙動）。
全トークンを環境変数にすれば自動起動できるが、平文でディスクに載る。

# 関連

- [テスト戦略](/quality/test-strategy.md) — このスキルが埋める「手動チェック」の範囲
- [リリース前手動チェックリスト](/quality/release-checklist.md) — 目視項目の元
- [Slack セットアップ Quickstart](/operations/slack-quickstart.md) — 本番向けの導入手順
- [ADR-0032 herdr protocol 17](/decisions/adr-0032-herdr-protocol-17.md) — この検証で見つかった非互換
