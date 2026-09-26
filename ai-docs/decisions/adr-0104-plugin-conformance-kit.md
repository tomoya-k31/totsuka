---
type: Decision
title: ADR-0104 プラグインの約束事を、実バイナリを黒箱で検査する適合キット plugin-conformance に 1 か所化する
description: 各プラグインの tests/ に揃わない形で複製されていたプロトコル適合テストを、プラグインのバイナリを起動して stdio で検査する新クレート plugin-conformance に集約した決定。in-process 検査・plugin-sdk / test-support への同居は却下し、kind ごとのリクエスト一覧は plugin-protocol の HOST_REQUESTS に置いてメソッド定数の網羅をテストで保証する。
resource: https://github.com/tomoya-k31/totsuka/issues/767
tags: [decision, plugin, protocol, testing, conformance, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-26T18:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable（#767）。設計の議論は issue #767 のコメントにある。

# Context

プラグインがプロトコルの約束事を守っているかの検査は、各プラグインの `tests/` に複製されていた。中身を突き合わせると揃っていなかった。

- `methods_before_initialize_are_rejected`（herdr / orca / github / notion）は、orca だけがエラーコードを 6 メソッドで見ていた。残り 3 本は 1 メソッドを投げ、メッセージに "initialize" が含まれるかしか見ていない
- `an_unknown_trigger_key_fails_initialize`（github / notion / slack / discord）は、`CONFIG_INVALID` を見ていたのが discord だけ
- 同じ約束事が別名でも書かれていた（slack の `malformed_and_notification_lines` など）

#759 で公式プラグインが `plugin-sdk` のハンドラに載り、汎用挙動（initialize 前の拒否、PARSE_ERROR、METHOD_NOT_FOUND、空行・通知への無応答、shutdown）は SDK が 1 か所で実装するようになった。各プラグインの in-process テストは同じ SDK コードを何度も試す一方で、**そのバイナリが SDK に正しく配線されているか**は誰も見ていなかった。notifier-macos は SDK のハンドラに載っていない。外部のプラグイン開発者に渡せる検査も無かった。

# Decision

1. **継ぎ目は黒箱にする。** キットはプラグインのバイナリを子プロセスとして起動し、NDJSON を書いて応答を読む。プラグインの内部型には触れない
   - in-process（`LineHandler` を受け取る形）は、SDK に載らない notifier と Rust 以外の外部プラグインを取りこぼすので却下
   - 制約として、`initialize` の成功を要しない経路だけを検査する。成功した後は実 API に触れる。その経路は各プラグインの in-process テストが持つ
2. **置き場所は新クレート `plugin-conformance`**（`publish = false`、git の dev-dependency で使う）。依存は `plugin-protocol` だけで、arch-lint の `conformance-deps` が検査する
   - `plugin-sdk` への同居は却下。テスト用のプロセス起動が本番の依存に入り、検査する側とされる側が同じクレートになって SDK の誤りを打ち消しうる
   - `test-support` への同居は却下。git / scratch のヘルパと混ざり、外部に渡すと不要なものまで付いてくる
3. **公開 API は `check(binary, plugin.toml, &InitializeParams) -> Vec<String>`**。kind と capability はマニフェストから読む（引数で別に渡すと食い違う余地が生まれる）。`InitializeParams` は `initialize` が通る最小の形で、task_source ならワークフローを 1 つ含める。キットはこれを直接は送らず、壊した複製（形の崩れた params、未知の config キー、未知のトリガーキー）だけを送る。全項目を実行して違反を全部返す。項目ごとにテストを生成するマクロは、外部開発者にとって読みにくく壊れたときに追いにくいので採らない
4. **検査は 9 項目**（一覧は [plugin-conformance](/components/plugin-conformance.md)）。エラーは**コードだけ**を見る。メッセージの文言はプロトコルの約束事ではない。例外は未知キー（検査 6・9）で、エラーがそのキー名を含むことを要求する。キー名は、運用者が設定を直すための唯一の手がかりだからである。応答と終了の待ちは固定の 5 秒（正常なら数ミリ秒）
5. **kind ごとのリクエスト一覧は `plugin-protocol` の `methods::HOST_REQUESTS` に置く**。各エントリは capability の条件（`sent_to`）を持つ。`tests/host_requests.rs` は `method` の全定数をソースから読み、表か除外リスト（共通 / P→O / 通知）のどれかにちょうど 1 回載っていることを検査する。一覧をキットの中に持つと、メソッドを足したときに「足し忘れても何も落ちない」状態がそのまま残る
6. **汎用の in-process テストは、キットで置き換えて消す**。プラグイン固有の検査は残す（例: slack の `initialize_rejects_malformed_config` は `initialize` 側の未知 config キー検査で、キットの検査 6 は `config/validate` 側なので残す）。**ヘルパの集約（`init_params`・`call` など）はしない**。`init_params` はプラグイン固有で、issue が数えた `mock_plugin`・`read_log`・`workflows` は orchestrator 側のテストのものだった
7. **マニフェスト互換の重複検査は消す**。`plugin-protocol` の `bundled_manifests` テストが同梱の全マニフェストを既に検査している
8. **2 層のスタック PR にする**。L1 はキット・protocol の表・arch-lint・slack への適用。L2 は残り 6 本への適用と複製の削除、`plugin-dev-guide` での外部公開

# Consequences

- **キットは初回の実行で SDK の不具合を 1 つ見つけた**。`plugin_sdk::runtime::serve` は `shutdown` の応答を writer タスクに積んだまま戻り、`main` の終了とランタイムの破棄に負けて応答が落ちていた（手元で 5 回中 4 回）。in-process テストは `Reply` の値を見るだけなので、これを捕まえられなかった。`Stdio::flush()` を足し、`serve` が戻る前に呼ぶようにした
- **残り 6 本へ適用したとき、さらに 2 種類のずれが見つかった**。discord は initialize 前の `task/update_status` に成功を返していた。no-op だからといって、initialize 前の拒否まで省く理由はなかった（同じ no-op の slack は拒否している）。herdr と orca は `config/validate` で serde のエラーを捨てて `config does not parse` だけを返しており、どのキーが悪いのかが伝わらなかった。どちらも他のプラグインに揃えた
- **消したテストの基準**は「すべての assert をキットが肩代わりしている」こと。1 つでも固有の assert があれば残した。たとえば github / notion / slack の未知トリガーキーのテストは、有効なキーの候補（`status`・`label`・`reaction` など）を挙げることまで見ているので残し、それが無い discord の分は消した。消したテストの doc コメントが運んでいた根拠は、キットの検査項目の説明へ移した
- 検査の有効性は、各項目の期待値を 1 つずつ反転させ、そのたびに slack の適合テストが落ちることで確かめた（14 通り、全部落ちた）
- nextest のテスト数は減る。「数の一致」はこの作業の検査にならないので、PR 本文に「消したテスト → 肩代わりした検査項目」の対応を書く
- 外部のプラグイン開発者は、git の dev-dependency 1 行と数行のテストで同じ検査を使える。Rust 以外で書く人には、9 項目の一覧がプロトコル上の約束事の一覧として残る

# 関連

- [plugin-conformance](/components/plugin-conformance.md)
- [plugin-protocol](/components/plugin-protocol.md)（`HOST_REQUESTS`）
- [plugin-sdk](/components/plugin-sdk.md)（`Stdio::flush`）
- [ワークスペース依存境界ルール](/architecture/workspace-dependency-rules.md)（`conformance-deps`）
- [テスト戦略](/quality/test-strategy.md)
