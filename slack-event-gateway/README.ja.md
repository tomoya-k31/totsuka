> 🌐 [English](README.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

# Slack Event Gateway

Slack からの配信を HTTPS で受け、**座標**に射影して、利用者の Pub/Sub トピックへ
publish する。totsuka は起動しているあいだにそのトピックを引く。

この間接化そのものが目的である。Socket Mode では `totsuka run` のプロセス自身が
WebSocket を握るので、**totsuka が止まっている間のメンションは失われる**。さらに
配信失敗が続くと、Slack はそのアプリのイベント購読を無効化し、復旧には人が Slack の
設定画面で操作するしかない。ここには、Slack が配信するために動き続けるものが無い。

設計は `ai-docs/decisions/adr-0072-slack-event-gateway.md` にある。

## このプロセスが絶対にしないこと

**本文を保存しない・転送しない・ログに出さない。** 本文に対してやってよいのは定数文字列との
一致判定だけである。メッセージを解釈するために外部 API を呼ばない（LLM を含め、何も）。
外向きの通信は Pub/Sub への publish 1 本だけで、それがこのプロセスの目的である。

レコードのスキーマには本文を運べるフィールドが無いが、**それは保証ではない** ——
プロセスは文字列をメモリに保持することも、出力することも、別の宛先へ送ることもできる。
保証はコードと、それを固定するテスト（`tests/http.rs`）のほうである。

## なぜこのリポジトリにありながら workspace の外なのか

ルートの `Cargo.toml` がこのディレクトリを除外している。`plugins/` には置けない ——
アーキテクチャ lint がそこのメンバー全部に totsuka プラグインであることを要求する ——
し、`crates/` に入れると、大半の人が一生デプロイしないサービスが全員の
`cargo build --workspace` に乗る。

代償はプラグインと型を共有できないことである。`contracts/slack-event-gateway/` が
あるのはそのためで、そこの適合ケースが両者の合意のすべてであり、双方が独立にそれを
満たすことを証明する。**このゲートウェイをフォークした実装も、スイートを通せば適合している。**

## 関門

Slack は IAM プリンシパルになれず、許可リストに使える安定した送信元 IP の一覧も公開して
いない —— 推測で組んだ許可リストは配信を落とし始め、それはまさにこの仕組みが直そうと
している失敗そのものである。したがってこのコンテナの手前には層が無く、公開インターネットとの
あいだに立つのは次の 3 つだけである。

1. **推測不能なパス。** `/slack/e/<opaque-token>` を利用者ごとに 1 本。パスが「誰の
   signing secret で検証するか」を選ぶ。本文中の識別子で引く案は「検証前の本文を信じて
   鍵を選ぶ」形になるので採らない。
2. **署名。** 生のボディに対する HMAC-SHA256 を、定数時間で比較する。
3. **タイムスタンプの窓。** 5 分。署名は「そのボディが Slack から来たこと」を証明するが、
   キャプチャされたリクエストが永久に使えてしまうのを止めるのは窓だけである。

未登録のパスと署名不正には**同じ答え**を返すので、パスを総当たりする価値が生まれない。

## 設定

| 変数 | 意味 |
|---|---|
| `REGISTRATIONS_PATH` | 登録表を置いたファイル（Secret Manager のマウント）。**推奨** |
| `REGISTRATIONS` | 登録表を直接。簡単だが、全利用者の signing secret がリビジョンの設定に載る |
| `PORT` | 待ち受けポート。Cloud Run が設定する。既定 8080 |
| `PUBSUB_URL` | Pub/Sub のベース URL。テスト用 |

登録表は利用者ごとに 1 行:

```json
{
  "users": [
    {
      "path_token": "<推測不能な文字列>",
      "slack_user_id": "U0123456",
      "signing_secret": "<Slack アプリの Basic Information ページから>",
      "topic": "projects/<project>/topics/<operator>-events",
      "block_actions_topic": "projects/<project>/topics/<operator>-presses"
    }
  ]
}
```

トピックが 2 本あるのは保持期間が違うからである。ボタン押下は `response_url` の
30 分程度の寿命を超えて保持する必要があるがそれ以上は要らず、メッセージのほうは日単位で
保持する。両方に同じトピックを書いた表は起動時に拒否される。

## Request URL は 2 つあり、両方が要る

Slack の設定項目は**2 つ**あり、どちらもここへ向ける必要がある。

| Slack アプリの設定 | 運ぶもの |
|---|---|
| Event Subscriptions → Request URL | メンション、リアクション |
| Interactivity & Shortcuts → Request URL | ボタン押下（`block_actions`） |

Socket Mode ではどちらも同じ WebSocket で届いていたので、前者だけを設定してしまいやすい ——
症状は「**メンションは動いたまま、承認ボタンとリポジトリ選択ボタンだけが一切届かない**」になる。

## 公式イメージと、差し替え方

totsuka のリリースごとに `ghcr.io/tomoya-k31/totsuka/slack-event-gateway:<tag>` を公開する。
タグは**ビルド元の totsuka のバージョン**である。OpenTofu モジュールの既定値がこれなので、
手でビルドするものは何も無い。

**`:latest` は無い。** モジュールは正確なバージョンを固定する —— 浮動タグがあると
`tofu apply` が黙って動いているものを入れ替えられるようになり、Slack の signing secret を
持つサービスでその性質は持ちたくない。

自前でビルドして使うには（会社での利用なら通常そうすべきである。自分たちが管理する
レジストリからイメージが来る形になる）:

```bash
cd slack-event-gateway
docker build -t <your-registry>/slack-event-gateway:<tag> .
docker push <your-registry>/slack-event-gateway:<tag>
```

あとはモジュールの `image` 変数をそこに向ける。ビルドは引数もシークレットも取らない ——
プロセスが必要とするものはすべて実行時に環境から届く。

ベースイメージは 2 つともダイジェストで固定してあり、タグはそれぞれのコメントに書いてある
（GitHub Actions を SHA で固定するのと同じ理屈）。更新は手動で、手順は
`ai-docs/development/dependency-hygiene.md` にある。

## テストの回し方

```bash
cd slack-event-gateway
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`tests/conformance.rs` はリポジトリルートの `contracts/slack-event-gateway/` を読むので、
完全なチェックアウトの中で実行すること。
