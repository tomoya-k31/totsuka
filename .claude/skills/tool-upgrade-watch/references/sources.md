# 更新情報の取得元

`scripts/fetch-sources.sh` が取りに行く先と、それぞれの癖。2026-10-17 に全 URL の到達を確認した。
URL が 404 / 移転になったら、ここと script を同時に直す。

## 共通の読み方

- **判定の根拠は「現行ドキュメント」、changelog は「いつから使えるか」を引くための索引。**
  changelog の箇条書きは自由文で、機能名が docs と一致しないことが多い。changelog だけで
  「追加された」と判断せず、必ず docs の該当節を読んで仕様（入力 JSON・スコープ・制約）を確かめる。
- ファイルは大きい（Claude の `changelog.md` は約 1 MB、`docs-hooks.md` は約 270 KB）。
  丸ごと読まず、`grep -n -i -E '<キーワード>'` で行を当ててから前後を読む。

## Claude Code

| 用途 | 取得元 | 備考 |
|---|---|---|
| ローカル版 | `claude --version` | |
| 最新版 | `npm view @anthropic-ai/claude-code dist-tags --json` | `latest` と `stable` がある。Homebrew cask は `stable` を追う |
| 追加機能（正典） | <https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md> | `## 2.1.NNN` 節ごとに "Added …" / "Fixed …"。ほぼ毎日リリース |
| ページ一覧 | <https://code.claude.com/docs/llms.txt> | 新しいページが増えたかはここで分かる。`docs.claude.com/llms.txt` は API 側へ転送されるので使わない |
| hooks | <https://code.claude.com/docs/en/hooks.md> / `hooks-guide.md` | 各イベントの入力 JSON・出力 JSON・matcher |
| settings / CLI | `settings.md` / `cli-reference.md` | `--settings` 以外の起動フラグ・環境変数 |
| plugins | `plugins-reference.md` | |
| permissions / sessions | `permissions.md` / `permission-modes.md` / `sessions.md` | deny ルールの照合、plan mode、別 cwd からの resume |

## Codex CLI（openai/codex）

| 用途 | 取得元 | 備考 |
|---|---|---|
| ローカル版 | `codex --version` | 未インストールのこともある（その場合は最新版だけで判定する） |
| 最新版 | `npm view @openai/codex version` | dist-tags は alpha のプラットフォーム別タグで汚れているので使わない |
| 追加機能（正典） | `gh api 'repos/openai/codex/releases?per_page=100&page=N'`（N=1..4）の `prerelease == false` | タグは `rust-v0.NNN.0`。100 件中 stable は 10 本前後しかないので、4 ページで基準版 0.145.0 まで届く。stable の本文は `## New Features` / `## Bug Fixes` で PR 番号付き |
| 次版の予告 | <https://raw.githubusercontent.com/openai/codex/main/CHANGELOG.md> | 中身は `## Unreleased` だけ |
| ページ一覧 | <https://learn.chatgpt.com/docs/llms.txt> | **docs は developers.openai.com から learn.chatgpt.com へ移転済み**。`curl -L` で転送を追う |
| hooks | <https://learn.chatgpt.com/docs/hooks.md> | repo の `docs/hooks.md` は存在しない |
| config | <https://learn.chatgpt.com/docs/config-file/config-reference.md> | `hooks.json` 以外の設定経路（`-c` 上書き、profile 等） |
| CLI | <https://learn.chatgpt.com/docs/developer-commands.md?surface=cli> | 起動フラグ |
| 設定層 | `config-file/config-basic.md` / `config-advanced.md` | `-c` 上書きと設定層の優先順位はここにしか書かれていない |

## opencode（anomalyco/opencode）

**2 系列が並走している。** `sst/opencode` は `anomalyco/opencode` へ転送される。

| | OpenCode 1 | OpenCode 2 |
|---|---|---|
| branch | `dev` | `2.0` |
| npm | `opencode-ai` | `@opencode/cli` |
| GitHub Releases | あり | **無い**（tag のみ） |
| docs | <https://opencode.ai/docs/> | <https://opencode.ai/v2/docs> |

- ローカル版は `opencode --version`。**入っている方の系列を見る**（script は版の先頭で branch を選ぶ）。
- v1 の docs の正典は repo の
  `https://raw.githubusercontent.com/anomalyco/opencode/dev/packages/web/src/content/docs/<page>.mdx`
  （`plugins` / `config` / `cli` / `agents` / `permissions`）。`plugins.mdx` の `### Events` 節が
  プラグインから購読できるイベントの一覧。
- **v2 の docs は <https://opencode.ai/v2/docs/>`<page>`（HTML）を読む。** repo の `2.0` branch の `.mdx` は
  v1 の API のまま更新されておらず（v2 に無い `session.idle` が載っている）、判定を誤らせる。
- changelog は <https://opencode.ai/changelog>（HTML・両系列混在）と、v1 の GitHub Releases。
  v2 で「いつから」を正確に引きたいときは `2.0` branch の `release: v2.0.N` コミット間の差分を見る。
- llms.txt は無い（404）。v2 docs の `.md` 版も 404 なので、v2 は上のとおり HTML を読む（repo の `.mdx` は v1 用）。
- プラグイン API の版は `@opencode-ai/plugin`（totsuka の JS プラグインが import する側）。
