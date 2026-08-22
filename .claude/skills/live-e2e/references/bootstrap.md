# 初回セットアップ（別の環境で一から作る）

`$E2E_HOME` がまだ無い環境で実機 E2E を始めるための手順。**人間の作業と自動化できる作業が
混ざる**ので、区分を明記してある。所要は 30〜60 分（大半は Slack アプリの作成と承認待ち）。

## 全体像

| # | 作業 | 誰が |
|---|---|---|
| 1 | ツールの導入（herdr / claude / op / gh / rumdl） | 人間 |
| 2 | Slack ワークスペースとチャンネル、B アカウント | 人間 |
| 3 | Slack アプリ 3 つとトークン 5 本 | 人間 |
| 4 | GitHub のサンドボックス repo 2 つと ProjectsV2 | **自動**（`scripts/github.sh bootstrap`） |
| 5 | `.env` の作成 | 人間（雛形は `assets/env.sample`） |
| 6 | `$E2E_HOME` の構築とプラグイン install | **自動**（`scripts/bootstrap.sh`） |
| 7 | `tt doctor` で確認 | 自動 |

---

## 1. ツールの導入【人間】

```bash
brew install herdr 1password-cli
# claude / gh は導入済み前提。無ければ各公式手順で
herdr --version   # 0.7.5 以上（protocol 17）が必要
herdr status      # protocol: 17 を確認
```

**herdr は 0.7.5 未満だと `initialize` が拒否する**（ADR-0032 D-6）。`herdr update` で上げる。

herdr のセッションを起動しておく（`herdr` を実行してターミナルに常駐させる）。ソケットが
`~/.config/herdr/herdr.sock` に出る。

## 2. Slack の器【人間】

- ワークスペース: 専用を作るのが理想。個人ワークスペースの流用も可（下記の注意）
- チャンネル `#totsuka-e2e` を作り、**A（運用者本人）と B（送信者役）の両方を参加させる**
- **カスタム絵文字を 1 つ登録**する（例 `:totsuka-test:`）

> **個人ワークスペースを流用する場合の注意**
>
> `[[workflows]].trigger.reaction` に `eyes` のような日常で使う絵文字を設定してはいけない。普段どおり
> 👀 を付けた瞬間にタスクが起動する。**専用のカスタム絵文字**にすること。
> また `message.channels` は A が参加する全公開チャンネルの投稿が流れてくる。

**B アカウントが要る理由**: メンション経路は「送信者が本人でない」ことが条件（判定表②）。
かつ **API 投稿には `bot_id` が付いて弾かれる**ので、B が**手で**打つ必要がある。

## 3. Slack アプリとトークン【人間】

**アプリは 3 つ**。被テスト用と、テスト駆動用（A 用・B 用）を分ける。被テストアプリに
駆動用のスコープ（`reactions:write`）を足すと、本番と違う権限のアプリを検証することになるため。

### アプリ 1: 被テスト（totsuka 本体が使う）

1. <https://api.slack.com/apps> → Create New App → **From a manifest**
2. リポジトリの `plugins/task-source-slack/manifest.yml` を貼る
3. Install App → Install to Workspace
4. **OAuth & Permissions** から `xoxp-…`（User）と `xoxb-…`（Bot）を控える
5. **Basic Information → App-Level Tokens** で `connections:write` スコープの `xapp-…` を作る
   （**この画面を閉じると再表示できない**。閉じたら作り直す）

> **マニフェストは必ず現行のものを使う。** `reactions:read` は #319 で追加された。それ以前の
> マニフェストで作ったアプリは、リアクショントリガを設定しても**無症状で動かない**
> （Slack がイベントを配送せず、エラーも出ない）。

### アプリ 2: テスト駆動（A がインストール）

**From a manifest** に `assets/driver-a.manifest.yml` を貼る。`xoxp-…` を控える。

### アプリ 3: テスト駆動（B がインストール）

**B のアカウントでログインした状態で** `assets/driver-b.manifest.yml` から作る。`xoxp-…` を控える。

> B がアプリを作れない場合は、ワークスペース設定でメンバーのアプリインストールを許可するか、
> A が作って **Settings → Collaborators** に B を追加し、B が Install to Workspace を押す。

### 控えるもの

| 値 | 取り方 |
|---|---|
| A のメンバー ID（`U…`） | プロフィール → … → メンバー ID をコピー |
| B のメンバー ID（`U…`） | 同上 |
| チャンネル ID（`C…`） | チャンネル詳細の最下部、または `scripts/slack.sh channels` |

## 4. GitHub のサンドボックス【自動】

```bash
bash .claude/skills/live-e2e/scripts/github.sh bootstrap
```

作られるもの:

- `totsuka-sandbox-web` / `totsuka-sandbox-cli`（private・Actions なし・**依存ゼロの stdlib unittest**）
- 各リポジトリの `README.md`（先頭 30 行が LLM 分類の材料）と `CLAUDE.md`（ブランチ規約・テスト手順）
- ProjectsV2「totsuka e2e」と `Status`（Todo / In Progress / Done）
- seed Issue 5 件（新規追加型。既存コードの書き換え課題にすると 2 回目の実行で判定がぶれる）

**2 つ作るのは、リポジトリ選択（LLM 分類 → picker）を検証するため。** 候補が 1 件だと
`repo_resolver` の ① 段で即確定して LLM にも picker にも到達しない。**説明文のドメインを
はっきり離す**こと（似ていると confidence が閾値を割り、分類が効いているのか縮退なのか
区別できなくなる）。

`pytest` ではなく標準ライブラリの `unittest` を使う理由: dispatch のたびに新しい worktree が
切られるため、そこで毎回パッケージ解決が走る構成は検証のノイズになる。

### GitHub トークン【人間の判断】

ProjectsV2 の書き戻し（F-84）には **project write** が要る。3 択:

| 案 | 内容 |
|---|---|
| **gh の OAuth を流用**（最短） | `gh auth refresh -s project` して `export E2E_GH_TOKEN="$(gh auth token)"`。新規クレデンシャル不要だが、主資格情報そのもので権限を絞れない |
| classic PAT | `repo` + `project`。アカウント全 repo に効くので期限 30 日にして使い終わったら削除 |
| fine-grained PAT | **user 所有の Project には使えない**（アカウント権限に Projects が存在しない）。org 所有にすれば `Projects: Read and write` が使える |

## 5. `.env`【人間】

`assets/env.sample` をリポジトリ直下の `.env` にコピーして値を埋める。**`.env` は
`.gitignore` 済み**であることを確認する。

**`XDG_CONFIG_HOME` を `export` してはいけない。** `gh` が `$XDG_CONFIG_HOME/gh` を読むため、
シェル全体の認証が壊れる。雛形の `tt()` は `env` で totsuka の起動時にだけ被せている。

## 6. `$E2E_HOME` の構築【自動】

```bash
source .env
bash .claude/skills/live-e2e/scripts/bootstrap.sh
```

やること: ディレクトリ作成 → `cargo build --workspace` → プラグイン（slack / github / herdr /
mock_agent）を `$E2E_HOME` のストアへ install → `assets/` の設定を配置（**既存は上書きしない**）
→ サンドボックスのクローン。

## 7. 確認

```bash
source .env && tt config validate && tt doctor
```

`state-db` の fail は `run` 前なら正常。`plugin:slack` が skip になるのは、**doctor が
`op://` の解決を促さない設計**だから（非対話を保つため）。実際の疎通は `run` で確認する。

## トークン一覧（最終形）

| 用途 | 種別 | 置き場所 |
|---|---|---|
| 被テスト・本人名義 | `xoxp-` | 1Password（`op://…/user_token`）または `.env` |
| 被テスト・Socket Mode | `xapp-` | 同上 |
| 被テスト・ナッジ DM | `xoxb-` | 同上 |
| 駆動・A（`reactions:write` 等） | `xoxp-` | `.env` の `E2E_SLACK_A` |
| 駆動・B（`chat:write`） | `xoxp-` | `.env` の `E2E_SLACK_B` |
| GitHub | OAuth or PAT | `.env` の `E2E_GH_TOKEN` |
| フック認証 | 任意文字列 | `.env` の `E2E_HOOK_TOKEN` |

> **1Password を使うと、常駐プロセスは人間のターミナルからしか起動できない。**
> 全部 `.env` の環境変数にすればエージェントからも起動できるが、トークンが平文でディスクに載る。
> 検証専用ワークスペースなら後者も妥当な選択。
