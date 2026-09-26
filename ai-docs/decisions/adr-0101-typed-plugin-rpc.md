---
type: Decision
title: ADR-0101 プラグイン呼び出しの型の対応を plugin-protocol の型付きメソッド記述子に閉じる
description: "core と doctor がプラグインを呼ぶとき、メソッド名と params/result 型の組を呼び出し箇所ごとに手書きしていた。これを plugin-protocol の rpc モジュール（trait Method とメソッドごとのゼロサイズ型）に 1 箇所で閉じ、Plugin::request::<M> で呼ぶようにした決定。issue 原案の kind ごとのクライアント trait と、Engine のインメモリフェイク化は却下・切り出した。"
resource: https://github.com/tomoya-k31/totsuka/issues/757
tags: [decision, plugin, protocol, core, typing, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-26T18:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable（#757）。

# Context

`Engine` と `doctor` は、プラグインを汎用の `Plugin::call<P, R>(method, &params)` で呼んでいた。メソッド名は `plugin_protocol::method::*` の定数だが、どの定数にどの params 型・result 型が対応するかは呼び出し箇所ごとの手書きで、コンパイラは検査しない。型を取り違えてもビルドは通り、実行時の serde エラーになる。4 つのメソッドは result を `serde_json::Value` で受けて捨てており、それが意図なのかどうかもコードから読み取れなかった。

同じ対応表はプラグイン側（`plugin-sdk` の dispatch）にも手書きであり、どちらも正本ではなかった。

`orchestrator-core` の doc は、存在しない `TaskSource`・`AgentIde`・永続化 port を挙げていた。

# Decision

1. **対応は `plugin-protocol` の `rpc` モジュールに置く。** `trait Method { const NAME: &str; type Params: Serialize; type Result: DeserializeOwned; }` と、O→P リクエスト 13 メソッドごとのゼロサイズ型（`TaskDispatch` など。params/result 型名から接尾辞を外した名前）。`NAME` は既存の `method::*` 定数を参照する。定数は残す（プラグインの server が文字列で match しているため）
2. **呼び出しは `Plugin::request::<M>(&M::Params) -> Result<M::Result, HostError>` にする。** core・cli の全 O→P リクエストと、ホスト内部の `initialize` / `config/validate` をこれに移した。名前と型を手で組にする `call` は `pub(crate)`（`request` の実装用）に下げ、呼び出し元の無い `call_no_params` は削除した
3. **result が未定義の 4 メソッド（`task/update_status`・`result/publish`・`task/cancel`・`state/subscribe`）は `serde::de::IgnoredAny` にする。** 既存のプラグインは `null` を返し、外部プラグインが `{}` などを返してもプロトコル違反ではない。`()` は `null` しか受け付けないので振る舞いが変わる。`IgnoredAny` ならすべてを受け付け、読まないことを型で表せる
4. **wire は不変なので `PROTOCOL_VERSION` は上げない。** 追加は公開モジュール 1 つだけで、既存の利用者は壊れない

# Alternatives considered

- **kind ごとのクライアント（`TaskSourceClient` / `AgentIdeClient` / `NotifierClient`）を core に置き、trait にする（issue 原案）**: 却下。対応を core に閉じても sdk 側の手書きは残り、「1 箇所」にならない。protocol crate はワークスペースの葉なので、core・cli・sdk・プラグインのどこからでも同じ記述子を参照でき、`arch-lint` の境界も変わらない。差分も記述子のほうが小さい
- **クライアントを trait にして、`Engine` のテストをインメモリのフェイクで書けるようにする**: 切り出した。`Engine` がプラグインに頼っているのは要求／応答だけではない。`notify`、`state/subscribe` の通知ストリーム、`task/submit`・`task/lookup` の P→O 受信、liveness と launch spec による再起動監視（#495）、`stats`、`shutdown` にも頼っている。要求／応答だけを trait にしても Engine は動かず、全部を trait にすると #495 の監視に手が入る。フェイクが必要な具体的なテストが出てきた時点で扱う
- **`PluginSet` の launch spec を 1 つの構造体に寄せる**: 型付けとは無関係なので対象外（別 map にしている理由は `PluginSet` の doc にある）

# Consequences

- params / result 型を変えると、影響する呼び出し箇所をコンパイラが列挙する。呼び出し箇所から turbofish の型指定が消えた
- 記述子と名前の組み合わせの誤りだけはコンパイラに見えないので、`rpc` の単体テストが全記述子の `NAME` を wire の文字列と突き合わせる
- `shutdown`（params も result も持たず、応答を待たないベストエフォートな要求）、`notify` 通知、P→O の要求、sdk の dispatch はまだ記述子を使っていない。記述子は protocol crate にあるので、後から同じものに寄せられる
- `ports` と crate の doc は、実在する 8 つの port（`AgentSession`・`Clock`・`GitRunner`・`RepoClassifier`・`SecretStore`・`SignalPort`・`ControlPort`・`ProcessProbe`）に合わせた。プラグインと永続化には port を置かないことを明記した
