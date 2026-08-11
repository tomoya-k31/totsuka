---
type: Decision
title: ADR-0041 pane には答える人が居ないので、3 ツールとも承認を求めない設定で起動する
description: "無人 pane が承認プロンプトで止まる問題（#420）に対し、claude は permissions.defaultMode = auto、codex は --ask-for-approval never、opencode は --auto で起動する決定。境界は deny / サンドボックス / deny マップが別に持っており、この設定はそれを緩めない。より厳しい dontAsk を採らなかったのは、allow リストの取りこぼしが「タスクが仕事をできない」に静かに化けるため。claude 側は profile がある workflow に限る。"
resource: https://github.com/tomoya-k31/totsuka/issues/420
tags: [decision, security, permissions, claude-code, codex, opencode, profile, adr]
generated: { by: claude-code/opus-5, at: 2026-08-11T23:10:00+09:00 }
status: stable
verified: [{ by: claude-code/opus-5, at: 2026-08-11T22:50:00+09:00 }]
owner: tomoya-k31
sources:
  - id: issue-420
    resource: https://github.com/tomoya-k31/totsuka/issues/420
    title: "確認: plan フラグ撤去後、triage / design の pane が既定モードで承認プロンプトに止まらないか"
  - id: claude-permissions
    resource: https://code.claude.com/docs/en/permissions
    title: Claude Code — Configure permissions
  - id: opencode-permissions
    resource: https://opencode.ai/docs/permissions/
    title: opencode — Permissions
---

# Status

stable（[#420](https://github.com/tomoya-k31/totsuka/issues/420)）。[ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md) が plan ゲートを外したことで開いた穴を塞ぐ。

# Context

[ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md) D2 で全 read-only profile から claude の `--permission-mode plan` を外した。無人 pane を 14 分止めた `ExitPlanMode` の承認ゲートを消すためである。

**停止は消えたのではなく、場所が移っただけだった。** plan フラグを外した pane は環境の既定モードで起動する。totsuka が `--settings` で配っているのは `permissions.deny` **だけ**で、`allow` も `defaultMode` も書いていない。

実測（Claude Code v2.1.227、totsuka が実際に生成した `orchestrator-github-design.json` をそのまま `--settings` に渡し、PTY 上で起動）:

- ユーザー設定から `permissions.allow` / `ask` / `defaultMode` を取り除いた config ディレクトリでは、pane は **`manual` モード**で起動した（`manual` は v2.1.200 以降の CLI での `default` の表示名）
- `defaultMode: "default"` を明示した状態で allowlist に無い Bash コマンドを頼むと、こうなった:

  ```text
  Bash command
     awk 'BEGIN{print 42}'
  This command requires approval
   Do you want to proceed?
   ❯ 1. Yes   2. Yes, and don't ask again for: awk *   3. No
  ```

  **そのまま止まった。** 無人 pane なら誰も押さないので、[#409](https://github.com/tomoya-k31/totsuka/issues/409) とまったく同じ形の停止である。

開発機で再現しなかったのは、そのマシンのユーザー設定に `Bash(gh:*)` などの広い `allow` と `defaultMode = "auto"` があったからである。

**同じ問題を codex と opencode も持っている。** codex の implement 既定は `--ask-for-approval on-request`（「モデルが必要と判断したら人に聞く」）で、plan 既定は `--sandbox read-only` だけ＝承認ポリシーは codex の既定にフォールバックしていた。opencode は何も指定しておらず、`bash` / `edit` は既定 allow だが `doom_loop` / `external_directory` は `ask` である。

# Decision

## D1. 3 ツールとも「人間に確認を求めない」設定で起動する

| ツール | 綴り | 置き場所 |
|---|---|---|
| claude | `permissions.defaultMode = "auto"` | `--settings` のファイル |
| codex | `--ask-for-approval never` | plan / implement 両方の既定 argv |
| opencode | `--auto` | plan / implement 両方の既定 argv |

綴りは違うが、決めていることは同じ **「pane に答える人が居ないのだから、止まって聞くな」** である。

## D2. **境界は別の機構が持つ。この設定はそれを緩めない**

ここが D1 を安全にしている全部なので、根拠を明示する:

- **claude**: deny ルールは**どの permission mode でも適用される**（[Claude Code のドキュメント](https://code.claude.com/docs/en/permissions)、`permissions.rs` の module doc にも以前から記録がある）。profile の deny セットは `auto` でも `default` と同じ強さで効く
- **codex**: `--sandbox` は承認ポリシーとは**別のフラグ**である。両方まとめて捨てる `--dangerously-bypass-approvals-and-sandbox` が第 3 のフラグとして存在することが、独立している証拠になる。`--sandbox read-only --ask-for-approval never` が解析を通ることは実機で確認した（`--ask-for-approval` の可能値は codex 自身が `untrusted, on-request, never` と列挙する）
- **opencode**: `--auto` の説明は CLI 自身の `--help` にあり、**「auto-approve permissions that are not explicitly denied」**。plan エージェント `totsuka-plan` の `edit/bash/task: deny`（[ADR-0023](/decisions/adr-0023-configurable-prompt-surface.md) で設定不可に固定した deny マップ）はそのまま残る

変わるのは、**境界が拒否しないもの**について人間に聞くかどうかだけである。

## D3. より厳しい `dontAsk` は採らない

claude には `dontAsk` — 「`permissions.allow` で事前に許可されていないツールを、**プロンプトを出さずに拒否**する」モード — がある。実測でも、allowlist に無い `awk` はプロンプト無しで拒否され、allowlist にある `gh` は実行された。**無人で止まらず、しかも fail-closed** である。

それでも採らない。`triage` / `design` に `gh issue comment` を実行させるには profile ごとの `allow` リストが要り、**そのリストが少しでも足りないと「プロンプトが出ない」が「タスクが仕事をできない」に、静かに、コマンド単位で化ける**。`auto` は逆側に倒れる。

これは意図的な取引である: **境界は `deny` であって mode ではない。** mode に境界を担わせようとすると、`Bash(...)` パターンで塞ごうとして失敗した [#410](https://github.com/tomoya-k31/totsuka/issues/410) と同じ形になる — 網羅できない列挙に安全の名前が付く。

## D4. claude 側は `profile` がある workflow に限る

`permissions` ブロックはもともと profile がある workflow にしか書かれていない（[ADR-0033](/decisions/adr-0033-workflow-profile.md) D4）。`defaultMode` も同じ門を通す。

理由は deny のときと**逆向き**である。deny を素の `mode = "plan"` に広げると既存の構成が黙って**厳しく**なるので広げなかった。`defaultMode = "auto"` を広げると既存の構成が黙って**緩く**なる — 人間が承認していた呼び出しが自動承認になる。どちらも「アップグレードで挙動が変わる」ので、**profile への移行がその選択への opt-in である**という線をそのまま使う。

`implement` profile は deny セットを持たないが、`defaultMode` は書く（`permissions` オブジェクトに `defaultMode` だけが入る）。無人で回したいのは implement も同じである。

## D5. `mode_args` / `plan_args` の明示は既定を丸ごと置き換える

これは既存の作法どおりで、変更していない。運用者が argv を書いたならそれを尊重する。**結果としてこれらのフラグも消える**ので、リファレンスにその旨を書いた。

# 不採用案

## totsuka の settings に `allow` リストを書く（`default` のまま）

[#420](https://github.com/tomoya-k31/totsuka/issues/420) 本文の第 1 案。allow は前方一致なので、`&&` 連結が allow 側から漏れる。ただし Claude Code は現在 `&&` `||` `;` `|` `|&` `&` と改行をコマンド区切りとして認識し、**allow ルールは各サブコマンドすべてに一致しないと通さない**とドキュメントにある。つまり漏れる方向は「自動承認されない」＝ 停止で、危険側ではなく**止まる側**に倒れる。D3 と同じ理由で採らない。

## `defaultMode` を書かず、環境に任せる

現状維持。開発機では動き、クリーンな環境では止まる。**「動くかどうかがマシンの個人設定に依存する」状態そのものが #420 の中身**なので、これを残す選択はない。

# Consequences

## 良くなること

- `triage` / `design` / `implement` が、どのマシンでも同じように無人で走る
- 「動いた／止まった」がツールの個人設定に依存しなくなる
- 3 ツールで**性質が揃う**。これまで codex だけ `on-request` で、opencode は何も指定していなかった

## 引き受けたコスト

- **`auto` は fail-open である。** 境界（deny / サンドボックス / deny マップ）が拒否しないものは、人間に聞かずに実行される。境界の質がそのまま安全性の質になる
- **claude の `auto` が持つ「background safety checks」の中身は totsuka から制御できず、未実測**である。Claude Code の説明を信じている
- **codex の `never` は「実行失敗をモデルに返す」** 挙動で、承認で止まる代わりにサンドボックスに弾かれた失敗をモデルが読むことになる。ループに入る可能性は未実測
- **opencode の `--auto` は CLI が "(dangerous!)" と表示する。** 表示どおり、deny を書いていない領域は無防備である
- profile を持たない素の `mode = "plan"` / `mode = "implement"` の workflow は**この恩恵を受けない**（D4）。無人で回したい運用者は profile へ移行する必要がある

# 検証

- `cargo test --workspace --all-features` — 全 profile が `permissions.defaultMode = "auto"` を書くこと、`implement` は `deny` キーを**持たない**まま mode だけ書くこと、素の `mode = "plan"` には `permissions` キーが**付かない**こと、codex が plan / implement 両方で `--ask-for-approval never` を持ちかつ `--sandbox` を失わないこと、opencode が両モードで `--auto` を持ち plan では `totsuka-plan` も保つこと
- **claude の挙動は実機で測った**（v2.1.227、PTY）: 個人設定から `allow` / `ask` / `defaultMode` を除くと `manual` 起動になること、`default` では allowlist 外の Bash が承認プロンプトで停止すること、`dontAsk` ではプロンプト無しで拒否され allowlist 内は実行されること、そして **`--settings` から `defaultMode` を設定できること**（ユーザー設定の `auto` を `default` / `dontAsk` に上書きできた）
- **codex / opencode はフラグの受理を実機で確認した**（codex-cli 0.145.0 / opencode 1.18.4）。`--sandbox read-only --ask-for-approval never` は解析を通り、`--auto` は `opencode --help` に "auto-approve permissions that are not explicitly denied (dangerous!)" として存在する
- **未検証**: `auto` で実際に無人 design タスクが完走すること（実機 E2E）、codex `never` のループ挙動、opencode の `doom_loop` / `external_directory` が `--auto` でどう扱われるか
