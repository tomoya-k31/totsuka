---
type: Decision
title: ADR-0116 config.toml を対話で組み立てるスキル totsuka-config を npx skills で配布する
description: "config.toml を 1 段ずつ確認しながら組み立てるエージェントスキル skills/totsuka-config を、このリポジトリに同梱して npx skills（vercel-labs/skills）で配る決定。スキルは config の知識を持たず、config schema（x-help / x-secret / x-by-source）に聞いて config set で 1 オブジェクトずつ書き、config validate で検査する。秘密は x-secret のキーすべてに secret:<ドットでつないだキーパス> を書き、値はメニューバーアプリが Start で尋ねる（スキルは値に触れない）。最終確認はアプリの Start に引き渡す。通知プラグインは書かない。これにより ADR-0113 で使い道が無いとされた config schema / set / unset に利用者ができたので残す。"
resource: https://github.com/tomoya-k31/totsuka/issues/903
tags: [decision, skills, config, secrets, menubar, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-17T04:30:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: vercel-skills
    resource: https://github.com/vercel-labs/skills
    title: vercel-labs/skills（npx skills）
  - id: agent-skills-spec
    resource: https://agentskills.io/specification
    title: Agent Skills specification
---

# Status

stable。#903 で実装した。実機検収（隔離した XDG で新規作成から Start まで、既存への追加、`npx skills add` での導入）は #903 のチェックリストで行う。

# Context

`config.toml` を書くのが難しい。`totsuka setup` が書き出すのは全行コメントアウトの雛形（約 600 行）で、どのキーをどの順で埋めるか、ソースごとに何が必須か、どの profile がどのソースで動くかは、[config-reference](/development/config-reference.md) を読まないと分からない。参照の連鎖（`[plugins.<name>]` → `[[projects]].source` → `[[repositories]].project` / `[[workflows]].projects`）があるので、書く順番も決まっている。

一方で、対話しながら 1 段ずつ書く道具はすでに CLI にある。[config CLI](/apis/config-cli.md)（`config schema` / `get` / `set` / `unset`）は、外した設定画面のために作られた（[ADR-0113](/decisions/adr-0113-native-menubar-app.md)）。スキーマには各キーの説明（`x-help`）、秘密の印（`x-secret`）、ソースごとの project のキー（`x-by-source`）が入っている。ただし ADR-0113 の時点で、これらは「同梱のどこからも使われない。使い道が無いまま残るなら、消すかどうかを改めて決める」とされていた。

# Decision

1. **`skills/totsuka-config/` に置き、`npx skills` で配る。** `npx skills add tomoya-k31/totsuka --skill totsuka-config` で入る。npm への publish や登録は要らない（public リポジトリの SKILL.md を CLI が直接取る）[^vercel-skills]。リリース tarball と Homebrew formula は変えない。`.claude/skills/` の開発用スキルには `metadata.internal: true` を付け、一覧から外す（#906）。
2. **スキルは config の知識を持たず、CLI に聞く。** キーの名前・必須・説明は `config schema` から取り、書き込みは `config set`、検査は `config validate` で行う。スキルが持つのは、スキーマに書けないことだけ（書く順番、人間が totsuka の外でやること、値の調べ方、罠、profile とソースの組み合わせの可否）。これでスキルは、利用者の手元の版の totsuka に合わせて動く。必要な下限は 0.10.5（`config set` が 0.10.4、アプリの「Use gh auth token」が 0.10.5）。
3. **雛形は使わない。** `plugin install --bundled` でプラグインを入れ、`config set` で必要なものだけを書く。`set` は雛形のコメント行を有効化しないので、雛形の後に書くと、コメントと実体が二重に並ぶ。
4. **秘密は `secret:` に統一する。** `x-secret` の付いたキーには `secret:<ドットでつないだキーパス>`（例: `secret:slack.user_token`）を書く。`op://` / `cmd:` などは使わない。`secret:` と他の参照は 1 つの config に混ぜられない（`--secrets-stdin` の下では他のストアが拒否される、[ADR-0100](/decisions/adr-0100-secrets-stdin.md)）ので、統一しておけば、あとからソースを足しても書き換えが要らない。既存の config に他の参照があれば、`secret:` への置き換えを提案する。スキルは値を尋ねず、書かない。値はアプリが Start で尋ね、github のトークンは「Use gh auth token」で取る（[ADR-0114](/decisions/adr-0114-macos-app-gh-token.md)）。
5. **最終確認はアプリの Start に引き渡す。** スキルが実行するのは `validate --offline` と、`secret:` を使わないプラグイン（herdr / orca）まで調べる `validate` まで。外部サービスに問い合わせる検査は、値を持つアプリが Start で行う。起動に失敗したら、スキルはアプリのメッセージを受け取って直す。
6. **通知プラグインは書かない。** 通知はアプリが `run --events-jsonl` から出す。既存の config にあるものには触れない。
7. **`config schema` / `set` / `unset` は残す。** このスキルが利用者になった。契約は [config CLI](/apis/config-cli.md) のまま変えない。

# 代替案と不採用理由

- **`.claude/skills/` に置く** — このリポジトリで開発しているセッションにも読み込まれて、誤って発火しうる。開発用スキルとの区別も internal の印だけに頼ることになる。
- **TOML をエージェントが直接編集する** — キーの正しさが、スキルに同梱した知識頼みになる。リリース版の totsuka とずれたときに検出できない。
- **`config set` が雛形のコメント行を有効化するよう改修する** — toml_edit の装飾を解析する必要があり、改修が大きい。雛形を使わなければ問題自体が起きない。
- **構成によって `cmd:gh auth token` と `secret:` を切り替える** — GitHub だけの config に Slack を足すときに、github.token の書き換えが要る。分岐も増える。
- **スキルが秘密の登録まで代行する／本人にターミナルで `--secrets-stdin` 付きの doctor を実行してもらう** — 値が会話ログやシェルの履歴に残る。
- **雛形を書き出す `totsuka setup` を対話化する（ADR-0028 の対話ウィザードへの回帰）** — #705 で置き換えた形に戻る。対話の分岐（ソース・profile の組み合わせ）を Rust で持つより、スキーマを読めるエージェントに任せるほうが、プラグインの追加に追従しやすい。

# Consequences

- 利用者は `npx skills add` 1 行でスキルを入れ、エージェントとの対話で config を作れる。Claude Code に限らず、`npx skills` が対応するエージェントで動く（SKILL.md はエージェント固有のツールに依存しない）。
- `config schema` の `x-help` / `x-secret` / `x-by-source` は、スキルが質問を組み立てる材料になった。プラグインがキーを足したら、スキーマに説明を付けることが、スキルの質に直結する。
- **`set` も `validate --offline` も、プラグインの節の中のキー名の綴り違いを通す。** プラグインのキーを検査するのはプラグイン自身（オンラインの検査）なので、スキルはキー名をスキーマからだけ取る。
- スキルは main から取得されるので、リリース済みの totsuka より新しいことがある。スキーマを手元の CLI から取ることで、キーのずれは吸収できる。スキルの手順が新しい CLI の機能を前提にするときは、SKILL.md の下限の版を上げる。
- `secret:` に統一した config は、ターミナルから `doctor` / `run` を実行しても秘密を解決できない（ADR-0113 の帰結のまま）。スキルはそのことを最初に伝える。

[^vercel-skills]: vercel-labs/skills（npx skills）
