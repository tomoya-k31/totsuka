> 🌐 [English](click-to-focus-setup.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/operations/click-to-focus-setup.md sha256:249a4afe70b35be017281ed4507c45195d52c5273a9a90e70659f153425bb99d -->

# 通知をクリックしてタスクの pane を開く

既定では、totsuka の通知をクリックすると**スクリプトエディタ**が開くだけで何も起きない。これは macOS 側の設定でどうにかなるものではない。既定のバックエンドは `osascript` 経由で通知を出しており、macOS は通知を `osascript` の持ち主に渡すためである。

`terminal-notifier` バックエンドに切り替えると、クリックでターミナルが前面に来て、その通知が指すタスクの pane がフォーカスされる。

所要 5 分程度。macOS 専用。

## 1. terminal-notifier を入れる

```bash
brew install terminal-notifier
```

## 2. ターミナルの bundle id を調べる

```bash
osascript -e 'id of app "Alacritty"'   # → org.alacritty
```

自分の使っているターミナルに読み替える。よくあるもの:

| ターミナル | Bundle id |
|---|---|
| Alacritty | `org.alacritty` |
| iTerm2 | `com.googlecode.iterm2` |
| Kitty | `net.kovidgoyal.kitty` |
| WezTerm | `com.github.wez.wezterm` |

## 3. `[macos]` テーブルを書く

設定ディレクトリ（`$XDG_CONFIG_HOME/totsuka/plugins/`、通常は `~/.config/totsuka/plugins/`）に置く:

```toml
[macos]
backend = "terminal_notifier"
activate_bundle_id = "org.alacritty"          # 手順 2 の値

# 以下は既定値。書かなくてよい:
# terminal_notifier_bin = "terminal-notifier"
# click_command = "totsuka focus {task_id}"
```

**テーブル名は `[macos]` である。`[notifier-macos]` ではない。** プラグイン名が付く。ここを間違えると `totsuka config validate` が弾く —— `[plugins.*]` に無い名前のトップレベルテーブルは受け付けない。

効くのは `backend` である。バックエンドが既定のままだと、`activate_bundle_id` だけ書いても何も変わらない。

`totsuka` バイナリは、terminal-notifier がクリック時に起動するシェルから見える `PATH` 上に要る（Homebrew・`~/.local/bin`・`/usr/local/bin` 等の標準的な場所なら通常問題ない）。

## 4. 確認する

```bash
totsuka config validate    # terminal-notifier の疎通も見る。未導入なら理由の分かるエラーが出る
```

そのうえで `totsuka run` を再起動する。プラグインは起動時に設定を受け取るので、動いたままのオーケストレータは古いバックエンドのままである。

実タスクを待たずにクリックを試すなら、totsuka が使うのと同じ引数で自分で通知を出せる:

```bash
terminal-notifier -title "test" -subtitle "click-to-focus" -message "click me" \
  -group "totsuka-1" -activate "org.alacritty" -execute "totsuka focus '1'"
```

ターミナルが前面に来れば成功。初回は macOS が terminal-notifier に通知の許可を求めることがあるので、許可する。

`totsuka run` を動かした状態で実際の通知をクリックすると、ターミナルの前面化とそのタスクの pane のフォーカスが両方起きる。複数のタスクが並走していても、それぞれの通知が自分の pane を開く。

## 効かないとき

| 症状 | 原因の候補 | 対処 |
|---|---|---|
| 通知は届くがクリックしても何も起きない | `backend` が既定の `osascript` のまま | `[macos]` に `backend = "terminal_notifier"` を書いて `totsuka run` を再起動する |
| 設定を書いたのに何も変わらない | テーブル名が `[notifier-macos]` になっている、または設定を `[macos]` ではなく `[plugins.macos]`（ロスター）へ書いている | `[plugins.macos]` はプラグインの宣言、`[macos]` はその設定である。名前を間違えたトップレベルテーブルは `totsuka config validate` が弾くので、黙って無視されることはない |
| アプリは前面に来るが pane が変わらない | `totsuka run` が動いていない（フォーカス操作は静かに何もしない）／pane が既に閉じている／使っているエージェントが pane の操作に対応していない | `totsuka focus <task-id>` を手で実行する。理由が表示される |
| クリックでコマンドは走るがアプリが前面に来ない | `activate_bundle_id` が未設定か誤り | 手順 2 で確認し直す |
| `config validate` が terminal-notifier のエラーを出す | 未導入／`PATH` 外／`terminal_notifier_bin` が誤り | 導入するか絶対パスを書く。使わずに済ませるなら `backend = "osascript"` に戻す（通知は届くがクリックは効かない） |
| 通知は届くがクリックが一度も効かず、ログに terminal-notifier の警告が出る | 未導入。送信ごとに `osascript` へフォールバックしている | 通知自体には影響しない。click-to-focus が要るなら導入する |
| `totsuka focus` が 401 を返す | 動いているイベント receiver と設定中の認証トークンが食い違っている | トークンを揃えて `totsuka run` を再起動する |

## どのイベントで通知するか

同じテーブルで、そもそもどのイベントを通知するかも決められる。既定は全部 on なので、**切りたいものだけ**を書く。

```toml
[macos]
backend = "terminal_notifier"
activate_bundle_id = "org.alacritty"

# 全ワークフローに効く
[macos.filter.events]
done = false
pending = false

# ワークフロー別の上書き。上のグローバル設定より優先される
[macos.filter.workflows.slack-reply]
done = true
```

イベント名は `waiting_input` / `done` / `failed` / `pending` / `escalated` / `verification_pending` の 6 つ。ワークフロー名は自分の `[[workflows]]` に書いた名前である。

**より具体的な指定が勝つ** —— ワークフロー別がグローバルより優先され、どこにも書かなかったイベントは通知される。書き間違えても黙って無視はされない。`totsuka config validate` が不明なイベント名を弾き、有効な名前を並べて表示する。

## 関連

新マシンの導入手順全体は [セットアップ Playbook](setup-playbook.ja.md) にある。

詳細な設計上の判断と、これらの手順の背景はリポジトリの `ai-docs/operations/click-to-focus-setup.md` にある。
