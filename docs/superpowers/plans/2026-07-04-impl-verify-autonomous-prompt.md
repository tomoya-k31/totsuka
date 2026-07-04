# Impl-Verify Autonomous Loop Prompt — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite `default_impl_verify_prompt()` so the implementer agent autonomously runs the full implement → PR → CI-watch → review-fix loop and moves the card to 「🚧 最終レビュー」 itself.

**Architecture:** Prompt-driven (approach A of the spec `docs/superpowers/specs/2026-07-04-impl-verify-autonomous-loop-design.md`). The only production change is one default-prompt function in `totsuka-config`; the orchestrator already spawns the agent on ImplVerify entry and `orchestrator::prompt::render` already expands all six placeholders. No orchestrator / watcher / adapter changes.

**Tech Stack:** Rust workspace; `cargo test -p totsuka-config`; CI-strict clippy.

## Global Constraints

- `#![forbid(unsafe_code)]` everywhere (no new code paths, but keep it in mind).
- Never call `Utc::now()` / `SystemTime::now()` in production code (not relevant here — no logic added).
- Lint gate before done (CI-strict, catches lockfile drift): `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` and `cargo fmt --check`.
- Commits: Conventional Commits scoped by crate → `feat(totsuka-config): ...`.
- Unattended commits must disable signing: `git -c commit.gpgsign=false commit ...`.
- Prompt text is Japanese, matching the style of `default_design_prompt()` (same file, lines 209-222): continuation-escaped string literal, `gh` CLI instructions, explicit card-move paragraph.

---

### Task 1: Rewrite `default_impl_verify_prompt()` with covering test

**Files:**
- Modify: `crates/totsuka-config/src/schema.rs:224-230` (function `default_impl_verify_prompt`)
- Test: same file, `mod tests` at `crates/totsuka-config/src/schema.rs:485` (append one test)

**Interfaces:**
- Consumes: nothing new — `default_impl_verify_prompt() -> String` already exists and is wired via `#[serde(default = "default_impl_verify_prompt")]` on `PromptsSection.impl_verify` (schema.rs:196) and `Default for PromptsSection` (schema.rs:204).
- Produces: the same signature, new template body. Placeholders `{repo}` `{issue_number}` `{branch}` `{task_id}` `{project_owner}` `{project_number}` are expanded by `orchestrator::prompt::render` (crates/orchestrator/src/prompt.rs) — use exactly these six, no others (an unknown placeholder would reach the agent verbatim).

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/totsuka-config/src/schema.rs` (after `missing_required_field_errors`):

```rust
    #[test]
    fn default_impl_verify_prompt_drives_autonomous_loop() {
        let p = default_impl_verify_prompt();
        // orchestrator::prompt::render が展開する 6 placeholder を全て含む
        for ph in [
            "{repo}",
            "{issue_number}",
            "{branch}",
            "{task_id}",
            "{project_owner}",
            "{project_number}",
        ] {
            assert!(p.contains(ph), "missing placeholder {ph}");
        }
        // issue はコメント込み・時系列で読む
        assert!(p.contains("--comments"));
        // PR↔task linkage の保険 (watcher の body トレーラー解決)
        assert!(p.contains("Closes #{issue_number}"));
        assert!(p.contains("Totsuka-Task: {task_id}"));
        // CI 監視ループ
        assert!(p.contains("gh pr checks --watch"));
        // 完了時はエージェント自身がカードを最終レビューへ移動、マージは禁止
        assert!(p.contains("🚧 最終レビュー"));
        assert!(p.contains("gh project item-edit"));
        assert!(p.contains("マージは行わない"));
        // エスカレーションは PR コメント報告 (カードは動かさない)
        assert!(p.contains("PR コメント"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p totsuka-config default_impl_verify_prompt_drives_autonomous_loop`
Expected: FAIL — first assertion to trip is `missing placeholder {project_owner}` (current template only has `{repo}` `{issue_number}` `{branch}`).

- [ ] **Step 3: Replace the template body**

Replace the whole function at `crates/totsuka-config/src/schema.rs:224-230` with:

```rust
fn default_impl_verify_prompt() -> String {
    "あなたは {repo} の実装・受入検証フェーズ担当です。以下の手順で進めてください。\n\
     1. コンテキスト把握: `gh issue view {issue_number} --comments` で issue \
     #{issue_number} の本文と全コメントを時系列に読んでください。設計フェーズが\
     投稿した設計コメントを含め、後のコメントほど新しい合意として優先します。\n\
     2. 実装: 現在のブランチ {branch} で実装し、このリポジトリのテスト・lint を\
     通した上でコミットしてください。\n\
     3. PR 作成: push して PR を作成してください。PR 本文に `Closes #{issue_number}` と\
     `Totsuka-Task: {task_id}` の行を含めてください。PR のマージは行わないでください。\n\
     4. CI 監視: `gh pr checks --watch` で CI を監視し、fail なら原因を修正して push し、\
     全チェックが green になるまで繰り返してください。\n\
     5. レビュー対応: CI green 後、10 分間を待機窓として 1〜2 分間隔でレビュー\
     (未解決レビュースレッド・PR コメント)をポーリングしてください。新規の指摘が\
     来たら返信・修正 push を行い(CI が再び green になることを確認)、待機窓を\
     仕切り直してください。10 分間新着がなく未解決スレッドが 0 になったら完了です。\n\
     6. 完了後: GitHub Project {project_number}(owner: {project_owner})の\
     このカード(item ID: {task_id})の Status を「🚧 最終レビュー」に\
     `gh project item-edit` で変更してください(field/option の ID は\
     `gh project field-list {project_number} --owner {project_owner}` で確認できます)。\n\
     エスカレーション: 修正を繰り返しても CI が通らない・指摘に対応しきれない場合は、\
     カードの Status は変更せず、状況を PR コメントで報告して終了してください\
     (PR 作成前に失敗した場合のみ issue #{issue_number} へのコメントで報告)。"
        .to_string()
}
```

Also update the doc comment on `PromptsSection` (schema.rs:190-191) — it currently lists only 4 placeholders; make it match reality:

```rust
/// Per-phase prompt templates sent to a freshly spawned agent.
/// Placeholders: `{repo}`, `{issue_number}`, `{branch}`, `{task_id}`,
/// `{project_owner}`, `{project_number}`.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p totsuka-config`
Expected: all tests PASS (the two existing config tests plus the new one).

- [ ] **Step 5: Workspace gate — test, clippy (CI-strict), fmt**

Run, in order; all must be clean:

```bash
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all
cargo fmt --check
```

Expected: tests pass (DB-gated tests skip silently without `DATABASE_URL` — that's normal), clippy zero warnings, fmt no diff.

- [ ] **Step 6: Commit**

```bash
git add crates/totsuka-config/src/schema.rs
git -c commit.gpgsign=false commit -m "feat(totsuka-config): impl_verify prompt drives autonomous PR/CI/review loop"
```
