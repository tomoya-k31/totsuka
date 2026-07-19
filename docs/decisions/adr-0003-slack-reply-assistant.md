---
type: Decision
title: ADR-0003 Slack メンション代理返信アシスタントの設計
description: task-source-slack をコア無変更のプラグイン内完結で実装する決定。リポジトリ解決はプラグイン内 3 段階、イベントはバッファ + 短周期 tasks/fetch、トークンはユーザートークン（xoxp）のみで本人名義返信 + 承認フロー必須。
tags: [slack, plugin, task-source, socket-mode, token, architecture]
timestamp: 2026-07-19T00:00:00Z
status: accepted
---

# Status

Accepted — 2026-07-15（エピック [#102](https://github.com/tomoya-k31/totsuka/issues/102)、実装 #103〜#108）

Decision §2（バッファ + 短周期 `tasks/fetch`）は [ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)（protocol 0.1.6 の `task/submit` push 取り込み）で amend された。

# Context

「自分宛の Slack メンションを totsuka のタスクにし、AI エージェントの返信案を自分の承認後に本人名義で返信する」機能（[task-source-slack](/components/task-source-slack.md)）を追加するにあたり、3 つの構造判断が必要だった:

1. **リポジトリ解決**: Slack のメンションには GitHub Issue のような「どのリポジトリの話か」という情報がない。コア（F-10/F-11 の repo_select）を拡張するか、プラグイン内で解決するか。
2. **イベント配送**: Slack Socket Mode は push 型だが、orchestrator のタスク取得は poll 型（`tasks/fetch`、F-06）。コアに push 経路を追加するか、既存の poll モデルに載せるか。
3. **トークン / 返信主体**: Bot ユーザーを作って Bot 名義で返信するか、ユーザートークン（`xoxp-`）で本人名義にするか。

# Decision

## 1. リポジトリ解決はプラグイン内 3 段階（コア無変更）

`[[channel_groups]]` prefix ルール（定義順 first-match）→ プラグイン内蔵の OpenAI 互換 LLM 分類（confidence 閾値）→ スレッド内エフェメラルでの手動選択、の 3 段階をプラグイン内で完結させる。確定した Task は常に `repo_hint` 付きで提出され、orchestrator は F-10 の hint 即決経路で処理する — コアの LLM repo_select には到達せず、orchestrator-core / plugin-protocol への変更はゼロ。

## 2. バッファ + 短周期 `tasks/fetch`（push 経路をコアに足さない）

Socket Mode で受けたメンションはプラグイン内バッファに正規化済み Task として積み、orchestrator は既存の poll モデル（`run --watch`）で吸い上げる。`[plugins.slack] poll_interval_secs = 5` を推奨し、体感遅延を数秒に抑える。プロトコルに push 通知を足す案は、全 task_source プラグインと Engine のイベントループに影響するため見送り（将来 F 案件として分離可能）。再起動でバッファは消えるが、メンションは Slack 側に残り再メンションで再投入できるため許容。

## 3. ユーザートークン（xoxp）のみ・Bot なし・本人名義返信 + 承認フロー必須

Slack アプリは Bot ユーザーを持たず、User OAuth Token（`xoxp-`）と Socket Mode 用 App-Level Token（`xapp-`）だけを発行する（[manifest 雛形](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml)）。返信は常に本人名義になるため、防波堤を 2 つ重ねる:

- **TokenGuard**（`initialize`）: `auth.test` の identity が `target_user_id` と一致しないトークンを拒否（他人のトークンでのなりすまし防止）し、`apps.connections.open` で `xapp-` トークンも起動時に検証する（`totsuka doctor` のプローブで両トークンの失効が見える）。
- **承認フロー**: エージェントの返信案は勝手に送信されず、スレッド内エフェメラル + self-DM 記録の 2 面に提示され、承認ボタン（confirm ダイアログ付き）押下時のみ送信される（[エフェメラル承認フロー](/glossary/ephemeral-approval.md)）。

トークンローテーションは無効（長命トークン）とし、保管は macOS Keychain に限定する（[運用ポリシー](/security/slack-user-token.md)）。

# Consequences

- スコープ変更時はアプリの再インストールが必要で、`xoxp-` トークンが再発行される（Keychain 更新 → `totsuka doctor` で検証）。手順は [Slack セットアップ Runbook](/operations/slack-quickstart.md)。
- プラグイン内 LLM 設定（`plugins/slack.toml` の `[llm]`）はコアの `[llm]` と独立している。リポジトリ候補が 1 件だけなら LLM 不要。（その後 #119 で default + override に発展: プラグインの `[llm]` 省略時はコアの `[llm]` が initialize で供給され default になる。明示時はプラグイン側が優先。`confidence_threshold` はフォールバック体験＝エフェメラル選択に紐づくためプラグイン側の意味論のまま）
- 再起動でバッファ・pending index・下書きストアは消える（in-memory）。下書きテキストは self-DM 記録に平文で残るため手動リカバリ可能。
- 全ループ（メンション → `tasks/fetch` → dispatch → `result/publish` → 承認 → 本人名義返信）はモック Slack + 実バイナリの E2E（`orchestrator-cli/tests/slack_e2e.rs`）で CI 検証される。この E2E が、Slack のタスク ID（`{channel}:{ts}`）が git ブランチ名として不正（`:`）という組み合わせバグを露見させ、コアの `render_branch` にサニタイズを追加した（ソース非依存の堅牢性修正としてのコア変更）。
- 「コア無変更」は #103〜#108 のエピック本体に対する判断であり、恒久の禁止ではない。設定重複（`[[repos]]` と `[[repositories]]`）の解消は、当初から任意 issue #109 として切り出したプロトコル拡張（`InitializeParams.repositories`、protocol 0.1.1 の追加的変更）で実施した。`[llm]` の重複（base_url / model）も同型の #119（`InitializeParams.llm`、protocol 0.1.2、default + override）で解消した。

# Citations

[1] [Issue #102 エピック / #108 運用整備](https://github.com/tomoya-k31/totsuka/issues/102)
[2] [task-source-slack コンポーネント](/components/task-source-slack.md)
[3] [Slack: Socket Mode / App manifest](https://api.slack.com/apis/connections/socket)
