---
type: Decision
title: ADR-0014 AI ツール抽象は「単一 pane runner + core 側ツールレジストリ + 解決済み ToolLaunchSpec」で行う
description: リポジトリ/ワークフローごとの AI ツール切り替え（#196、Claude Code / Codex / OpenCode）のため、agent プラグイン（pane runner）と AI ツール（pane 内 CLI）を直交 2 軸に分離し、ツール知識（argv 組立・ケイパビリティ・完了検知アダプタ）を orchestrator-core の [tools] レジストリに集約、protocol 0.2.3 の ToolLaunchSpec で完全解決済み argv/env をプラグインへ渡す決定。ツール別 agent プラグイン案と herdr 側プロファイル解決案は不採用。
tags: [tool, protocol, herdr, codex, opencode, registry, dispatch]
generated: { by: human:tomoya-k31, at: 2026-07-28T16:00:00Z }
status: stable
---

# Status

Accepted — 2026-07-24（[#196](https://github.com/tomoya-k31/totsuka/issues/196)。Phase 1 = 抽象と配線・claude のみ・挙動不変。先行 rename は #222）

# Context

herdr プラグインが `claude … --settings <path> [--resume <id>]` の CLI フラグをハードコードしており、pane 内で走る AI ツールが Claude Code に固定されていた。リポジトリやワークフローごとに Codex / OpenCode を使い分けたいが、「agent プラグイン（pane を管理する runner: herdr/orca）」と「AI ツール（pane 内で走る CLI）」の区別が config 上に存在しなかった。旧 `[[repositories]].default_agent` はこの 2 軸を混同した名残で、validate されるだけのランタイム未消費フィールドだった。

# Decision

1. **2 軸モデル**を導入する。`[[workflows]].agent` は従来どおり pane runner（agent_ide プラグイン）を選ぶ。新設の `tool` 軸（`[[workflows]].tool` > `[[repositories]].tool` > `default_tool` > 組み込み `claude`）が pane 内で起動する AI ツールを選ぶ。両者は直交し、herdr は全ツール共通の pane runner のまま。
2. **ツール知識は core に集約する**（`orchestrator-core::tool`）。`[tools.<name>]` レジストリ（`kind` / `command` / `mode_args` / `plan_args`）を `ToolProfile` に解決し、kind ごとの `ToolCapabilities`（不可視注入・marker block・prompt 検証・resume・plan・heartbeat・session id 捕捉）と argv 組立（`launch_spec`）を持たせる。herdr の `launch_command`（旧ハードコード）はここへ移設・一般化した（golden テストも移植）。
3. **protocol 0.2.3 の `TaskDispatchParams.tool_launch: Option<ToolLaunchSpec>`**（`program` / `args` / `env`、additive）で**完全解決済み** argv/env をプラグインへ渡す。プラグインは内容を解釈せず pane で起動するだけ（`HookLaunchSpec` / H-01 と同じ opaque contract 流儀）。`hook: Option<HookLaunchSpec>` は 0.2.3 から deprecated（移行窓の間は併送、次の breaking で削除）。herdr は `tool_launch` を優先し、無ければ旧 `launch_command` フォールバック（herdr.toml の `agent_command` / `plan_args` も deprecated）。
4. **完了検知は既存 UDS 契約に正規化するアダプタで吸収する**。`POST /agent-events` → `AgentSignal` → `Engine::on_signal` のワイヤ形は全ツール共通のまま（#222 の rename で名称も一般化済み）。アダプタを持たない kind（Phase 1 の codex / opencode、および任意 CLI）は全タスクが timeout エスカレーション終わりになるため、**validate で参照を拒否**し dispatch でも防御する（`ToolKind::has_adapter`）。`kind = "custom"`（アダプタ無し任意 CLI）は見送り確定 — ツール追加の正規ルートは「core に ToolAdapter 実装 1 つ + `[tools]` 設定 + 完了検知アセット」。
5. **`verification = "llm"` × tool は静的検証する**。llm 検収は Claude の prompt 型 Stop フック専用のため、非 claude 系へ解決されうる llm ワークフローには `tool = "claude"` のピンを警告で提案し、ピン済み不一致も警告する（実行時フォールバックは保険）。workflow レベル `tool` を v1 から導入した決め手はこの静的保証。
   - **実行時フォールバックの実体（[#301](https://github.com/tomoya-k31/totsuka/issues/301)、2026-07-28 に実装）**: `prompt_verification = false` のツールでは、完了信号の受信時に実効 verification を **`human` へ縮退**する（タスクは `Verifying` で止まり `totsuka task verify` を待つ）。縮退した回は run ログに warn を 1 回出す。
     この「保険」は ADR 制定時には**実装されていなかった** — `ToolCapabilities.prompt_verification` は宣言されただけでどこからも読まれず、`on_stop_completed` が `Llm` と `None` を同じ腕で扱っていたため、実際の縮退先は `human` ではなく **`none` 相当（未検証のまま publish）**だった。ケイパビリティは「宣言しただけでは縮退しない」ことの実例。
     実効値は永続化せず完了時にツールを引き直す（解決入力は `EngineSettings` にあり起動時から不変なので、同一プロセス内では dispatch 時の解決と一致する）。
6. **`default_agent` は削除する**。ランタイム消費が無く、`tool` の隣に「agent」名のフィールドが残ると 2 軸が再び混同されるため。`deny_unknown_fields` によりフィールド名入りのパースエラーになり移行は自明（pre-1.0・利用者ローカル設定のみ）。

## 不採用案

| 案 | 理由 |
|---|---|
| (A) ツールごとに agent プラグインを作る（agent-ide-codex 等） | pane 管理・deadman・snapshot 等の herdr 連携コードが 3 重化。pane runner × tool の N×M 爆発。`default_agent` と同じ軸の混同 |
| (B) herdr 側でツールプロファイル解決（protocol はツール名だけ渡す） | ツール知識（plan フラグ・resume 構文・hooks 注入）がプラグイン側に散り、別 runner が出るたび再実装。縮退制御用のケイパビリティ表は core に必須なので知識が二重管理になる |

# Consequences

- リポジトリ/ワークフロー単位で AI ツールを宣言的に切り替えられる基盤ができた。Phase 1 は claude のみ（挙動不変 — e2e で argv の従前同一性を固定）。
- **Phase 2 完了（2026-07-24、codex-cli 0.145.0 実機スパイク済み）**: [V1] Stop hook の exit 2 / `{"decision":"block"}` ブロック成立（R-03 同等）、[V2] Stop stdin に `last_assistant_message` 直載（ターンキーは `turn_id` → 送信スクリプトが `prompt_id` へ載せ替え）、[V3] plan permission mode は CLI に存在せず `--sandbox read-only` 縮退で代替、を確認して `kind = "codex"` を有効化。フックは既存スクリプト群の一般化（`TOTSUKA_JOB_ID` 早期ゲート）+ `$CODEX_HOME/hooks.json` グローバル登録（`hooks::codex` が構造マージで自己管理、trust は codex 側で一回きり承認 — [運用手順](/operations/codex-tool-setup.md)）で実現し、設計時想定の専用 on-codex-*.sh 群は不要だった。notify フォールバックスクリプト（設計 Phase 2 項目）は [V1] 成立により不要と判断し見送り。
- **Phase 3 完了（2026-07-24、opencode 1.14.39 実機スパイク済み）**: [U] `-s <session_id>` resume の文脈込み再開を確認して `kind = "opencode"` を有効化。完了検知は JS プラグイン（`session.status(idle)`/`session.idle` 両対応・`client.session.messages` で最終メッセージ取得・Bun fetch `unix:` で UDS POST）、plan は `--agent totsuka-plan`。スパイクの追加発見 2 件を実装へ反映: ① plan agent の `permission: {edit,bash: deny}` だけでは **General Agent へのサブエージェント委譲で編集が貫通** → `task: deny` を含む全 deny が必須。② 不可視注入チャネルが無いため、dispatch にケイパビリティ駆動のコンテキスト経路分岐（`caps.invisible_injection` ✗ → 可視 extra_context）を追加。全 kind がアダプタを持ち、`has_adapter` の拒否経路は将来の kind 追加に備えた残置となった。
- 将来の pane runner（orca 等）にもツール対応が自動で波及する（runner は `tool_launch` を起動するだけ）。
- thread 継続の resume は `caps.resume && caps.session_id_capture` でゲートされ、非対応ツールは常に新規 dispatch に縮退する。
- herdr.toml の `agent_command` / `plan_args` は後方互換フォールバックとなり、次の breaking protocol バンプで `hook` フィールドとともに削除予定。
- 検証済みツールバージョンの記録（#196 決定 9）は Phase 2/3 の実機スパイク時に `ai-docs/operations/` へ残す。
