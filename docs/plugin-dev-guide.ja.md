> 🌐 [English](plugin-dev-guide.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/development/plugin-dev-guide.md sha256:10a08a4a879fe06466ffe421c63ac0a51341b2212b52927432e70f3566a2d85d -->

# プラグイン開発ガイド

totsuka のプラグインの作り方。プロトコル、マニフェスト、kind ごとのメソッド、ビルドと導入のループを扱う。

## プラグインとは

プラグインは **stdio 上で JSON-RPC 2.0 を話す単一の実行バイナリ**で、1 行 1 メッセージ（NDJSON）でやり取りする。kind は 3 種類。

- `task_source` — タスクを供給する
- `agent_ide` — AI エージェントを駆動する
- `notifier` — 通知を配送する

プロトコルの唯一の正は `plugin-protocol` クレートで、必要な型はすべてそこが公開している。

## 依存

```toml
[dependencies]
plugin-protocol = { git = "https://github.com/tomoya-k31/totsuka" }
```

`Task`、`InitializeParams` / `InitializeResult`、各メソッドの params と result、`Manifest`、`Capabilities`、JSON-RPC のヘルパが使える。**プロトコルの版数はアプリ本体の版数とは独立している。**

## マニフェスト

各プラグインはバイナリと並べて `plugin.toml` を同梱する。

```toml
name = "github"                     # バイナリ名と一致させる
kind = "task_source"                # task_source | agent_ide | notifier
version = "0.1.0"                   # プラグイン自身の版
protocol_version = ">=0.6.0, <0.7"  # 対応する Orchestrator プロトコルの範囲

[capabilities]                      # 実装しているものだけ宣言する
state_stream = true                 # agent: 状態ストリーム対応
pane_control = true                 # agent: pane のフォーカス・解放・列挙
hook_completion = true              # agent: 完了をツールのフック経由で報告する
diagnostics_snapshot = true         # agent: diagnostics/snapshot に応答する
outputs = ["source"]                # result/publish を実装するときだけ宣言する。
                                   # 宣言しないと output = "source" の
                                   # workflow は弾かれる
```

Orchestrator は起動前に `protocol_version` の互換性を検査し、宣言された capability だけを要求する。

**Orchestrator が実際に読む鍵しか存在しない。** capability のフィールドと error code は「読み手がいるか」を機械検証しているので、何もしない鍵は追加できない。プロトコル 0.5.0 は読み手の無かった 5 つ（`plan_mode` / `task_submit` / `resume_session` と error code の `-32001` / `-32002`）を削除した。**これらが残っている古いマニフェストでも起動は失敗しない**（未知の鍵は無視される）。ただし `resume_session` は `hook_completion` に**置き換わった**ので、フック経由で完了を報告する agent は新しい名前で宣言し直すこと。

### 範囲の決め方

**上限**は、下回っていたい破壊的変更の**次**のメジャー／マイナーに置く（現行なら `<0.7`）。上限 `<0.3` のマニフェストは 0.3.0 の Orchestrator に、`<0.4` は 0.4.0 に、`<0.5` は 0.5.0 に、`<0.6` は 0.6.0 に、それぞれ起動を拒否される。

**下限も上限と同じくらい意味を持ち、決めるのは「何に依存しているか」である。** プラグインの kind でも、その時点の最新プロトコルでもない。

0.6.0 では同梱プラグインが結果として全部 `>=0.6.0` に揃ったが、それは全部が同じものに依存しているからである —— `initialize` が改名され、その呼び出しは kind を問わず全プラグインが読む。揃った結果を規則と読み違えないこと。

**揃わない例のほうが規則をよく表す。** 0.4.0 で herdr プラグインだけを `>=0.2.3` へ上げたのは、ツール起動に必要なフィールドが入ったのが 0.2.3 で、コマンドラインを自前で組み立てるフォールバックをもう持っていないからである。下限で弾いておくことが、削除したフォールバックを「非推奨」ではなく**到達不能**にしている。

同じ kind の orca プラグインは `>=0.1.0` のままだった。`orca` CLI を駆動していてそのフィールドを一度も読まないので、下限を上げると**問題なく動く Orchestrator を弾く**ことになる。

## メソッド

**O→P** は Orchestrator からプラグインへの呼び出し、**P→O** はその逆。

### 全 kind 共通

| メソッド | 方向 | 内容 |
|---|---|---|
| `initialize` | O→P | 解決済みの設定とプロトコル版を渡す。プラグインは自分の版と capability を返す |
| `config/validate` | O→P | プラグイン設定を検証する。`initialize` と同じ workflows / projects / repositories も一緒に届くので、記憶ではなく「今聞かれているもの」を検証する |
| `shutdown` | O→P | 猶予付きで終了を要求する |

`initialize` は `task_source` に対して、二重に設定せずに済むものをいくつか渡す。いずれも任意なので、使わないなら無視してよい。

- `repositories: [{name, summary?, path?}]` — Orchestrator 側のリポジトリ設定。ソース側でリポジトリを解決するプラグインは自前設定の重複を省ける
- `llm: {base_url, model, api_key?}` — Orchestrator 側の LLM 設定（鍵は解決済み）。プラグイン自身の LLM 設定があればそちらを優先し、これは既定値として扱う
- `workflows: [{workflow, trigger, instructions_kind?, task_id_prefix?, options}]` — 自分を `source` または `agent` として名指す workflow が、設定に書かれた順で届く。`trigger` はソースが監視する条件で、運用者が書いたとおり素通しで届く（agent には空オブジェクト）。`instructions_kind` / `task_id_prefix` は workflow の `profile` から Orchestrator が導出した値。`options` はその workflow に書かれた、Orchestrator が解釈しないキー。**自分が読まない `trigger` キーは拒否すること。** 素通しということは他の誰も検査しないということで、無視したキーは黙って捨てられ条件が消える —— タイポはトリガーを狭めず**広げる**。`plugin_sdk::unknown_trigger_keys(&init.workflows, TRIGGER_KEYS)` が未知キー 1 件につき1 メッセージ（自分が読む有効キー入り）を返すので、空でなければ `CONFIG_INVALID` で`initialize` を失敗させる
- `projects: [{name, options}]` — 自分が所有するプロジェクト（`source` が自分の `[[projects]]` エントリ）。各プロジェクトに紐づくリポジトリは `[[repositories]].project` から届く

ポーリング型ソースの取得周期はプラグイン自身の設定である: `poll_interval_secs` を自分の `[<name>]` テーブルに置き、`config` から読む。

### task_source

**task_source は push 専用である。** タスクを見つけたら `task/submit` を Orchestrator へ自分から送る。Orchestrator がタスクを取りに来る RPC は存在しない。イベント駆動のソース（Webhook や Socket）は受信のたびに送り、ポーリングが自然なソースは `initialize` で受け取った `workflows` と、自分の `[<name>]` テーブルの `poll_interval_secs` で自前のタイマーを回す。そのタイマーは `plugin-sdk` クレートが `poll_loop` として提供している。

| メソッド | 方向 | 内容 |
|---|---|---|
| `task/submit` | **P→O request** | 見つけたタスクを、**属する workflow を名指して** push する。受け取った workflow 群に対して first-match を走らせたのは自分なので既に分かっており、Orchestrator はその名前が実在して自分のものかだけを確かめる。Orchestrator は永続化してから応答する |
| `task/update_status` | O→P | タスクの状態遷移を伝える。ソース側へ反映する |
| `result/publish` | O→P | 成果物をソースへ書き戻す |

`task/submit` の応答は次の 3 つのいずれかで、**すべて最終**である。これらを理由に同じタスクを再送してはならない。

- `accepted` — 永続化された
- `duplicate` — 冪等キーが衝突した。破棄してよい
- `rejected` — 恒久的に処理できない（理由付き）

転送レベルのエラー（`NOT_ACCEPTING` / `SUBMIT_OVERLOADED` / `INTERNAL_ERROR`）は別で、submit は冪等なのでバックオフして再送してよい。

### agent_ide

| メソッド | 方向 | 内容 |
|---|---|---|
| `task/dispatch` | O→P | worktree 上で作業を開始し、セッション ID を返す |
| `task/cancel` | O→P | 実行中のタスクをキャンセルする |
| `session/attach` | O→P | 既存セッションへ再接続し、接続結果と現在状態を返す |
| `state/subscribe` | O→P | 状態とログのストリームを購読する |
| `state/notification` | P→O | 状態変化やログ断片を通知する |

**worktree は detached HEAD で渡される。** ブランチの作成、コミット、push、プルリクエストの作成はすべてエージェント側の責務であって、Orchestrator の仕事ではない。

`state` は `idle` / `running` / `waiting_input` / `done` / `failed` の 5 値。Orchestrator はこれを自身のステートマシンへ写像する（`running` で計測が始まり、`waiting_input` で並列枠が解放され、`done` で書き戻しへ進む）ので、自分のツールの実際の状態をこの 5 値へ正直に写像すること。

### notifier

| メソッド | 方向 | 内容 |
|---|---|---|
| `notify` | O→P（応答不要） | `waiting_input` / `done` / `failed` / `pending` のイベントを配送する |

通知は片道で応答を返さない。**配送に失敗してもタスクの実行に影響させてはならず**、失敗は自分のログに留めること。

## ログと stderr

**プラグインの stderr は Orchestrator のログにそのまま入る**（プラグイン名のタグ付き）。
デバッグには便利だが、**秘密を書かないのは作者の責務である** — Orchestrator は
プラグインが何を秘密と考えているか知らないので伏せられないし、プラグイン側から
Orchestrator の伏字処理には手が届かない。

転送は **10 秒あたり 100 行**に制限され、超えた分は「N 行抑制」の 1 行にまとめられる。
失敗ループに入ったプラグインは読む側より速く stderr を吐けるので、それで他のログが
埋まらないようにするためである。抑制した行数は報告されるので、うるささ自体は数字として残る。

Orchestrator からプラグインへの呼び出しは、Orchestrator 側でメソッド別に時間と回数が
記録される。`totsuka run --json` の `plugins` に、呼び出し数・結果の内訳・直近の
p50/p95 レイテンシが出る。プラグイン側で何かする必要はない。

## ビルドと導入

チェックアウトからなら、ビルド・install・enable が 1 コマンドで済む。

```sh
totsuka plugin install --from-source github --enable      # 1 つだけ
totsuka plugin install --from-source --all --enable       # 全部
totsuka plugin install --from-source --all --profile dev  # デバッグビルド
```

チェックアウトは現在地から上へ辿って自動検出する（`--repo <dir>` で明示も可）。判定は「Cargo ワークスペースのルートかつ `plugins/` を持つ」であって、git にトップレベルを尋ねる方法は使わない — 無関係なクローンの中でも答えてしまうためである。cargo は全対象パッケージに対して 1 回だけ起動する。何が起きるか先に見たいときは `--print-plan` を使う。

### 手作業でやる場合

各プラグインは `plugins/{crate}/` にあるワークスペースの通常メンバーなので、ワークスペースルートから対象を指定してビルドする。

```sh
cargo build --release -p task-source-github
```

生成物はクレート単体ではなく共有の `target/release/` に置かれる。

**バイナリ名は Cargo のパッケージ名ではなく `plugin.toml` の `name` である。** 各プラグインの `Cargo.toml` は `[[bin]] name` をマニフェストの `name` に合わせてあり（`task-source-github` パッケージのバイナリは `github`）、install が要求するのもこの名前なので、リネームは要らない。この一致は `scripts/arch-lint.sh` が機械的に検証する。合っていないと install は `plugin binary <name> not found in <dir> → expected a file named after the plugin` で失敗する。

ディレクトリを渡す形の install では、マニフェストとバイナリを同じ場所に置く必要がある。

```sh
mkdir -p dist/github
cp target/release/github plugins/task-source-github/plugin.toml dist/github/
totsuka plugin install ./dist/github
```

`--from-source` はこの中間ディレクトリを作らない。マニフェストはプラグインのソースディレクトリから、バイナリは `target/<profile>/` から直接読む。

## install と enable

- `totsuka plugin install <dir>` はディレクトリを検証し（確認用に SHA-256 を表示）、`$XDG_DATA_HOME/totsuka/plugins/{name}/` へ配置する
- `totsuka plugin enable {name}` は設定の `[plugins.{name}] enabled = true` を書き換える
- **バイナリを入れること（install）と設定で宣言すること（enable）は意図的に分けてある**

再インストールでも、**インストール先のバイナリを上書きすることはない。** 同じディレクトリに一時ファイルを作って rename で差し替えるので、インストール先は毎回新しい inode になる。macOS はコード署名の検証結果を vnode 単位でキャッシュするため、中身だけを書き換えると次回起動が無言で `SIGKILL` される。

## 参照実装

| kind | プラグイン |
|---|---|
| `task_source` | `task-source-github`（GraphQL）、`task-source-notion`（REST + プロパティマッピング） |
| `agent_ide` | `agent-ide-herdr`（Socket API アダプタ）、`agent-ide-orca`（CLI ラップ） |
| `notifier` | `notifier-macos`（osascript） |

最小の骨格としては `crates/orchestrator-core/src/bin/mock_plugin.rs` があり、設定駆動で全 kind を演じる。

## 設定の置き場所

プラグイン自身の設定は `config.toml` のトップレベル `[<name>]` テーブルである。`<name>` は `[plugins.<name>]` のロスター名 = バイナリ名。Orchestrator は中身を解釈せず、シークレット参照だけ解決して `initialize` の `config` として渡す。ロスターに無い名前のトップレベルテーブルは設定エラーになるので、タイポは黙って無視されずに報告される。

Orchestrator 側の構造体にキーを定義することもできる:

| 置き場所 | 所有の決め方 | 実装すること |
|---|---|---|
| `[[workflows]]` | **聞いて決める。** 余ったキーはその workflow の `source` と `agent` に届き、ちょうど 1 つが引き取る | `claimed_options` に `{workflow, key}` を返す。**消費しないキーを claim しない** —— タイポを沈黙に変える |
| `[[projects]]` | **`source` が決める。** エントリはちょうど 1 つのプラグインを名指す | `deny_unknown_fields` の構造体へデシリアライズするだけ。握手は不要 |

誰も引き取らない workflow のキーは起動を止める。2 つが引き取る場合も同じ。

## 動作確認

`totsuka config validate` は各プラグインの `config/validate` に委譲する（`--offline` を付けると静的検査だけになり、プラグインを起動しない）。`totsuka doctor` はライブで疎通を確認する。どちらでも、自作プラグインが起動して応答するかを確かめられる。

---

このページは内部ドキュメント `ai-docs/development/plugin-dev-guide.md` から生成されている。設計上の判断や実測の経緯はそちらにある。
