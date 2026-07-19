---
type: Decision
title: ADR-0006 シークレット参照に 1Password (op://) を第 2 バックエンドとして追加する
description: 設定のシークレット参照へ op://<vault>/<item>/<field> を第 3 のスキームとして追加し、解決は 1Password CLI（op read）へのシェルアウトで行う決定。SDK/Connect は不採用、v1 は対話アンロック前提（Service Account は後続）、SecretRef の enum 化 + 合成ストアでスキーム振り分け。op は cross-platform のため非 macOS 初の実働バックエンドにもなる。
resource: https://github.com/tomoya-k31/totsuka/issues/156
tags: [secret, 1password, op, keychain, config, backend, cross-platform]
timestamp: 2026-07-19T15:00:00Z
status: accepted
owner: tomoya-k31
---

# Status

Accepted — 2026-07-19（[#156](https://github.com/tomoya-k31/totsuka/issues/156)）

# Context

設定ファイル中のシークレット参照は macOS Keychain（`keychain:<service>/<account>`）と環境変数（`${ENV}`）の 2 形式のみだった。開発機のシークレットを 1Password で一元管理していても totsuka から直接参照できず、Keychain への複製か env への展開が必要だった。他アプリのエコシステムでは env ファイルに `op://` ネイティブ URI を書き `op inject` 系で解決するのが定着している。

# Decision

1. **`op://<vault>/<item>/<field>` を第 3 の参照スキームとして追加する**。`config.toml` / `plugins/{name}.toml` の任意の文字列 leaf で使え、既存の `keychain:` / `${ENV}` は挙動不変（後方互換）。解決は従来どおり **Orchestrator 側で一括**（F-65）: プラグインは解決済み平文のみ受け取り、`op` にもバックエンドにも触れない。
2. **解決手段は 1Password CLI（`op read --no-newline <uri>`）へのシェルアウト**。SDK / Connect は採用しない — `op inject` 系エコシステムと同一の参照をそのまま使え、外部依存が薄く、`op` が cross-platform なので **Keychain が `Unsupported` の非 macOS（Linux CI 等）でも初の実働シークレットバックエンド**になる（`platform/onepassword.rs` は `#[cfg]` ゲートなし）。
3. **認証は対話アンロック前提**（事前 `op signin` 済みセッション）。無人 Orchestrator 向けの Service Account トークン（`OP_SERVICE_ACCOUNT_TOKEN`）/ Connect 対応はスコープ外 = 後続 issue。
4. **型と振り分け**: `SecretRef` を enum 化（`Keychain { service, account }` / `OnePassword { uri }` — URI は `op read` が受理する native 形式のまま verbatim 保持）し、`PlatformSecretStore` を**合成ストア**に再定義して variant で委譲する（`Keychain` → Keychain/フォールバック、`OnePassword` → `OnePasswordCli`）。構文の eager 検証は追加しない従来方針を踏襲（`FromStr` は `vault/item/field` の形だけ要求し、存在検証は `op read` に委ねる）。
5. **エラーは「原因 + 次アクション」**（§7）: `op` 未インストール（spawn NotFound）→ 新設 `SecretError::BackendUnavailable`（`brew install 1password-cli`）、item 不在 → `NotFound`、未サインイン → `op signin` を提示。分類は **stderr のみ**から行い、stdout（= 平文シークレット）はログ・エラーに一切出さない（§5.2、`SecretString` の常時 `***` は不変）。テストは `Command` 実行を差し替える runner seam で実 `op` 非依存（CI グリーン）。
6. **doctor**: 設定に `op://` 参照が 1 つ以上あるときのみ `op --version`（存在）と `op whoami`（**非プロンプト**のセッション確認 — `op read` と違い biometric を誘発しない）を検査する。

# 代替案と不採用理由

| 案 | 不採用理由 |
|---|---|
| 1Password SDK（Rust crate） | 依存が重く、Service Account トークン前提で対話セッションを使えない。v1 の要件（開発機・対話アンロック）に合わない |
| 1Password Connect（セルフホスト API） | サーバ常駐が必要でローカル開発機のユースケースに過剰 |
| `op inject` / `op run` によるバッチ解決 | 設定全体を都度テンプレート展開する構造変更が必要。参照ごとの `op read` は実運用のトークン数が少数のため許容（キャッシュは将来最適化） |
| 参照構文の独自形式（`1password:<...>` 等） | ユーザーが既に持つ `op://` native URI をそのまま貼れる価値を捨てることになる |

# Consequences

- 非 macOS で初の実働シークレットバックエンドとなる（Keychain は従来どおり `Unsupported`）。
- 解決は参照ごとに `op read` を起動する（per-ref のプロセス起動コスト）。実運用のシークレット数は少数のため許容し、バッチ解決/キャッシュは将来の最適化とする。
- 対話セッションが切れていると解決は失敗する（エラーが `op signin` を案内）。無人運用は Service Account 対応（後続 issue）まで `keychain:` / `${ENV}` を使う。
- `SecretRef` の enum 化により、既存の `service()`/`account()` アクセサは variant マッチへ置き換わった（`NotFound` エラーも参照文字列ベースへ）。

# Citations

[1] [Issue #156 シークレット参照に 1Password (op://) バックエンドを追加](https://github.com/tomoya-k31/totsuka/issues/156)
[2] [1Password CLI — op read / secret reference syntax](https://developer.1password.com/docs/cli/reference/commands/read/)
[3] [設定リファレンス — シークレット参照](/development/config-reference.md) / [orchestrator-core](/components/orchestrator-core.md)
