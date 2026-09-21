---
type: Decision
title: ADR-0090 [tools.X].env_file の値を totsuka run の起動時に 1 回だけ解決し、エージェントの起動 env に渡す
description: herdr / orca が起動するエージェントに 1Password などの値を渡す手段が無かったため、[tools.<name>] に env_file を足した決定。書式は最小の dotenv サブセットで、それ以外は行番号付きのエラーにする。値は既存の SecretResolver で 4 スキームと ${VAR} を解決する。解決は totsuka run の起動時に 1 回だけで、ToolLaunchSpec.env に hook の有無にかかわらず入れる。TOTSUKA_ で始まる名前は拒否し、doctor は何も解決しない。
tags: [decision, config, tools, secrets, onepassword, adr]
generated: { by: claude-code/opus-5, at: 2026-09-21T17:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable（#744 の後半）。orca で値が画面に出ないことは [ADR-0089](/decisions/adr-0089-orca-env-fifo.md) が前提として担保する。

# Context

手元の Claude Code は `op run --env-file=~/.claude/.env.tpl -- claude` で起動し、1Password の値（API キー、コミット署名の `GIT_CONFIG_*` など）を環境変数で渡している。totsuka 経由のエージェントには、これを渡す手段が無かった:

- エージェントのプロセスを起動するのは herdr / orca のサーバーなので、`op run -- totsuka run` としても totsuka 自身の環境は届かない
- `ToolLaunchSpec.env` に入るのは totsuka が組み立てる `TOTSUKA_*` だけで、`[tools.<name>]` にも環境変数を書くキーが無かった
- `command = "op run … -- claude"` と書けば動くが、エージェントを起動するたびに 1Password の承認が挟まる

# Decision

1. **`[tools.<name>].env_file` を足す。** 全 kind で有効（環境変数は kind に依存しない概念で、`ToolLaunchSpec.env` も全 kind で共通の経路なので）。パスは既存の `expand_path` で `~` / `${VAR}` を展開し、**結果が絶対パスでなければエラーにする**。相対パスの基準（cwd か config のディレクトリか）が曖昧になるのを避けるため
2. **書式は最小の dotenv サブセット**: `KEY=value`、`#` のコメント行、空行、値の両端のクォート 1 組。`export`、複数行の値、`{{ }}` のテンプレート、重複キー、不正なキー名は、**行番号付きのエラー**にする。黙って捨てると、原因の分からない欠落になるため。エラーの文言に値は出さない。利用者の実ファイル（コメント、空行、クォート無しの値、ダブルクォートのリテラル）はこの範囲に収まる
3. **値の解決は既存の `SecretResolver::resolve` をそのまま使う。** `op://` / `keychain:` / `cmd:` / `bw:` をストアから取り、それ以外は `${VAR}` を展開する。他の `*_ref` と意味を揃え、スキームの判定を `is_secret_reference` の 1 か所に保つため（#699 の教訓）。代償として、リテラル値に `${` を書けず、`op run` とは `op://` 以外の扱いが違う。この差は設定リファレンスに書いた
4. **解決は `totsuka run` の起動時に 1 回だけ行い**、`env_file` を持つ `[tools]` エントリ**すべて**を対象にする。同じファイルは 1 回だけ読む。ワークフローやリポジトリから到達できる tool だけに絞ることはしない（tool の選択はリポジトリにも依存し、到達可能性を計算するのは割に合わない。承認は起動時の 1 回で済む）。hook ランタイムと同じ `!dry_run` のガードの中に置く。失敗したら起動を止め、値は run が持ち続けて読み直さない。結果は `EngineSettings.tool_env`（tool 名 → 解決済みの対応表）に入れる。hook のトークンと同じく、CLI が解決して engine に渡す
5. **`TOTSUKA_` で始まる名前はすべて拒否する。** 予約された 5 個だけではない。予約変数を後から足しても穴が開かず、エージェント側で動く `totsuka` が未知の `TOTSUKA_*` に警告を出す問題（[ADR-0009](/decisions/adr-0009-env-override-whitelist.md)）も避けられるため
6. **ディスパッチでは hook の有無にかかわらず `ToolLaunchSpec.env` に入れる。** 値は tool に属するからである。予約名との衝突はエラーにしてあるので、合流の順番は結果に影響しない。プロトコルは変えない（herdr にも orca にも既に届いている経路）
7. **`totsuka doctor` は何も解決しない。** 検査するのは、ファイルの存在、書式、スキーム付きの値が `SecretRef` としてパースできること、`TOTSUKA_` との衝突で、1 ファイル 1 行のチェック（`tool-env-file`）にする。値は数十個になり得るので、hook-token のように 1 個ずつ `deferred_note` で振り分けることはしない
8. ファイルのパーミッションは検査しない。普通は参照しか書かれておらず、`op run` も検査しないため

# 代替案と不採用理由

- **`op://` だけを解決する（issue の原案）** — `op run` と意味が揃う。しかし、この設定だけ他の `*_ref` とスキームの集合が違うことになり、判定の実装が 2 か所に分かれる
- **`command = "op run … -- claude"`** — 設定だけで動くが、起動のたびに承認が挟まる
- **解決済みの値を `ToolProfile` に持たせる** — `ToolProfile` は `PartialEq` を derive した純粋な設定の解釈で、`SecretString` は比較できない。解決は CLI の領分（シークレットストア）なので、hook のトークンと同じく `EngineSettings` の横に置いた

# Consequences

- 1Password の承認は、既存の `op://` と同じ起動時の 1 回で済む。エージェントの起動時には `op` を呼ばない
- 値を変えたら `totsuka run` の再起動が要る
- 解決した値は `totsuka run` のメモリに残る。エージェントの起動とは別の場所で 1Password を呼ぶもの（1Password の SSH エージェントによるコミット署名、orca の端末で読まれる rc の中の `op`）は止まらない
- 検証は 3 段で行う。パーサ・ローダ・解決の単体テスト（fake `SecretStore`）、mock プラグインが受け取った `tool_launch.env` を hook あり・なしの両方で確かめる結合テスト、doctor が解決しないことのテスト
