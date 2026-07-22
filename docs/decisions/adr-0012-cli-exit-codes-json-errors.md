---
type: Decision
title: ADR-0012 CLI の exit code 体系と --json エラーエンベロープ
description: CLI の exit code を 0/1/2/3（成功/実行時エラー/usage/doctor 問題検出）の名前付き定数として確定し、--json 指定時のエラーを stderr の 1 行 JSON エンベロープ {"error":{"message","action"}} で機械可読化する決定。特定 exit code は ExitWith 型の downcast で運び、既存の「原因 → 次のアクション」文字列規約をフィールド分割に再利用する。
tags: [cli, exit-code, json, error-handling, ux]
timestamp: 2026-07-23T13:00:00Z
status: accepted
---

# Status

Accepted — 2026-07-23（[#177](https://github.com/tomoya-k31/totsuka/issues/177)）

# Context

CLI は成功時の機械可読出力（`--json`、「parseable output on stdout, nothing else」契約）と「原因 → 次のアクション」のエラー文言規約を持つ一方、2 つのギャップがあった（[#177](https://github.com/tomoya-k31/totsuka/issues/177)）:

1. **エラーは常に非構造化テキスト** — `--json` を指定していても、エラー時は `error: ...` の平文が stderr に出るだけで、スクリプトや上位ツールがエラー種別・アクションを機械的に取り出せない。
2. **doctor の「問題検出」が汎用 exit 1 に潰れる** — 「doctor 自体の実行失敗」と「診断は完走し問題が見つかった」を exit code で区別できない。

exit code は `main.rs` に 0/1/2 の 3 値がハードコードされ、名前付き定数も docs 上の一覧もなかった。CLI のエラーは全経路が `CliError = Box<dyn Error>`（文字列ベース）で、種別を保持する型が存在しない。

# Decision

1. **exit code 体系を 4 値の名前付き定数として確定する**（`crates/orchestrator-cli/src/common.rs`）:

   | code | 定数 | 意味 |
   |---|---|---|
   | 0 | （`ExitCode::SUCCESS`） | 成功 |
   | 1 | `EXIT_ERROR` | 実行時エラー（より特定の code を持たない全エラー） |
   | 2 | `EXIT_USAGE` | usage エラー（サブコマンド無し / clap のパース失敗） |
   | 3 | `EXIT_PROBLEMS_FOUND` | 診断が完走し問題を検出（現状 `doctor` のみ） |

   特定 exit code は **`ExitWith { code, message }` 型**（`std::error::Error` 実装）で運び、`main` が `Box<dyn Error>` から downcast して code を取り出す。他のエラーは従来どおり `EXIT_ERROR`。`CliError` の文字列ベース設計・全コマンドのシグネチャは不変で、区別が必要な箇所（doctor の問題検出）だけが `ExitWith` を返す — エラー enum への全面移行はコストに見合わないため見送った。

2. **`--json` 指定時のエラーは stderr へ 1 行 compact JSON** `{"error":{"message":"<原因>","action":"<次のアクション>"|null}}` を出す。フィールド分割は既存の「原因 → 次のアクション」文言の**最初の ` → `** で行う（`action` はアクション連鎖全体、arrow 無しなら null）。stdout の「parseable output, nothing else」契約はエラー時も維持される（エラーは常に stderr）。非 `--json` 時は従来の `error: 原因 → アクション` 平文を維持する。

3. **`--json` フラグの宣言を共通化する** — 5 コマンド（status / task list / task show / plugin list / doctor）に重複していた `#[arg(long)] json: bool` を `common::JsonFlag`（`clap::Args`、`#[command(flatten)]`）に一元化。CLI 表面（受理するフラグ・ヘルプ）は不変。グローバルフラグ化（全コマンドで `--json` 受理）は、JSON 出力を持たないコマンドに無意味なフラグを生やすため不採用。あわせて `print_json` をバイパスしていた doctor / plugin list の JSON 出力も `common::print_json` に統一した。

# Consequences

- スクリプト・上位ツール（herdr 等のオーケストレーション文脈）は `--json` 時に stderr をパースしてエラー種別（message）と復旧手段（action）を機械判定できる。
- `totsuka doctor` は 0（問題なし）/ 3（問題検出）/ 1（doctor 自体の失敗）/ 2（usage）を区別して返す。CI や監視から「環境が壊れている」と「診断が動かない」を分けて扱える。exit 3 の意味は将来の他の診断系コマンドにも再利用可能。
- clap 自身のパース失敗（不正フラグ等）は clap 内部で exit 2 するため、メッセージ形式はプロジェクトの管理外のまま（既知の制約）。
- エラーの `message` / `action` 分割は文字列規約（` → `）依存であり、型レベルでは強制されない。文言に ` → ` を含めるかどうかが機械可読性に直結するため、エラー文言規約の重要性が上がる。
- exit code 一覧は [orchestrator-cli](/components/orchestrator-cli.md) の UX 規約節に明文化した。
