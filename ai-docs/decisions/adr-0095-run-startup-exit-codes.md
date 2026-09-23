---
type: Decision
title: ADR-0095 run の起動時エラーに専用の exit code（4 = 人が直すまで直らない / 5 = 既に稼働中）を割り当てる
description: ネイティブアプリが run を子プロセスとして監視・再起動するとき、設定・機密情報の誤りと一時的な障害がどちらも exit 1 で区別できず、再起動がループする。ADR-0012 の表を拡張し、run の起動工程で起きる「人が直さない限り再起動しても直らない」失敗（config・機密参照・env_file・プラグインの導入不備と CONFIG_INVALID）を exit 4、lock の競合を exit 5 とし、それ以外は従来どおり 1 とする決定。分類は run の起動工程の呼び出し元で ExitWith に包んで行い、main での型ダウンキャストは採らない。
tags: [decision, cli, exit-code, run, supervisor, adr]
generated: { by: claude-code/opus-5, at: 2026-09-23T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable（#755）。[ADR-0012](/decisions/adr-0012-cli-exit-codes-json-errors.md) を拡張する（置換しない）。

# Context

ネイティブの macOS アプリが `totsuka run --watch` を子プロセスとして起動・監視し、落ちたら再起動する構想がある（#753 / #754）。ADR-0012 の exit code は 0 / 1 / 2 / 3 で、`run` の実行時エラーはすべて 1 だった。そのため監視する側は次を区別できない。

- config・機密情報・プラグインの導入不備。人が直さない限り、次の起動も同じところで落ちる
- 一時的な障害。再起動すれば直る見込みがある

config を 1 箇所間違えただけで再起動が無限ループする。別の `run` が既に lock を持っている場合も 1 で、同じくループする。

# Decision

1. **exit code を 2 つ足す**（`crates/orchestrator-cli/src/common.rs`）:

   | code | 定数 | 意味 | 監視側 |
   |---|---|---|---|
   | 0 | （`ExitCode::SUCCESS`） | 正常終了 | 再起動しない |
   | 1 | `EXIT_ERROR` | より特定の code を持たない実行時エラー | 再起動してよい |
   | 2 | `EXIT_USAGE` | usage エラー | （監視下では起きない） |
   | 3 | `EXIT_PROBLEMS_FOUND` | doctor が問題を検出（`run` は返さない） | — |
   | **4** | **`EXIT_CONFIG`** | **`run` の起動時に、人が直すまで再起動しても直らない失敗** | **再起動しない** |
   | **5** | **`EXIT_ALREADY_RUNNING`** | **別の `run` が lock を持っている** | **再起動しない** |

   シグナルでの終了や表に無い code は「再起動してよい」。backoff や上限回数など再起動の方針は監視する側の責務で、ここでは決めない。

2. **4 の意味は「恒久的な故障」ではなく「人が直すまで再起動しても直らない」。** 監視する側が知りたいのは再起動に意味があるかどうかで、ロックされた vault や入っていないプラグインは恒久的ではないが自動では直らない。対象は `run` の起動工程の次の失敗:
   - config の読み込み・パース・検証、どのプラグインも持ち主にならない workflow キー（#554）
   - エンジン設定の組み立てとパス展開（`~` / `${VAR}`）
   - `[tools.X].env_file` の解決（パス・読み込み・中の参照の解決をすべて含む）
   - 機密参照の解決: プラグインの `[<name>]` 表（`plugin_spec` の失敗全般。未インストールも含む）、`[llm].api_key_ref`、`[hooks].auth_token_ref`
   - プラグインの起動: プロトコル版の不一致、spawn の `NotFound` / `PermissionDenied`、`initialize` が `CONFIG_INVALID`（-32003）を返したとき

3. **`SecretError` はバリアントを問わず 4。** `Backend`（`op` の未サインイン、Keychain のロック、`cmd:` の非 0 終了）も含む。ロックされた vault はループで再試行しても解錠されない。#754 が入ればアプリが解決済みの値を渡すので、backend の一時障害は実質起きなくなる。バリアントで分けないので規則が単純なまま保てる。

4. **lock の競合（`LockError::AlreadyRunning`）は専用の 5。** config の誤りではないので 4 と意味が違うが、再起動してもループするだけという点は同じ。code を分ければアプリは「既に稼働中」と区別して表示できる。lock の IO エラーは 1 のまま。

5. **それ以外は 1 のまま。** state DB を開けない、フックアセットの書き出しの IO エラー、`recover` の失敗、プラグインの spawn のその他の IO エラーと `initialize` 中のクラッシュ・タイムアウト、定常運転に入った後の `EngineError`。再起動で直る見込みがある。

6. **分類は `run` の起動工程の呼び出し元で行う。** 各呼び出しの失敗を ADR-0012 の `ExitWith { code, message }` に包み直す（`needs_fix` / `launch_error`）。`main` で型をダウンキャストする方式は採らない: `SecretError` などの型は定常運転中にも出うるので、型で分類すると起動時以外の失敗まで 4 にしてしまう。呼び出し元で包めば、どの呼び出しが対象かがコード上で明示される。メッセージの「原因 → 次のアクション」は変えない。

7. **番号は ADR-0012 の表の続き（4 / 5）。** sysexits.h（`EX_CONFIG` = 78 / `EX_TEMPFAIL` = 75）はリポジトリのどこでも使っておらず、既存の小さな連番に揃えたほうが 1 つの表として読める。

8. **`--json` のエラーエンベロープは変えない。** 分類は exit code が、理由は既存の `message` / `action` が表す。

9. **対象は `run`（`--dry-run` を含む）だけ。** 監視下で走るのは `run` だけで、`config validate` / `status` / `doctor` などは従来どおり 1 を返す。`--dry-run` も同じ呼び出し元を通るので、そこで起きる失敗（config の検証、プラグインの `[<name>]` 表の機密参照と起動、`[llm].api_key_ref`、エンジン設定）は 4 になる。ただし dry run はもともと `env_file` と `[hooks]`（`auth_token_ref` を含む）を解決しないので、それらの誤りは検出しない。lock も取らないので 5 は返さない。

# 代替案と不採用理由

- **`main` で型（`SecretError` / `ConfigError` など）をダウンキャストして分類する** — 起動後の同じ型まで 4 になり、再起動で直るはずの失敗で監視が諦める（Decision 6）
- **`SecretError::Backend` だけ 1 に残す** — 「一時的なネットワーク障害」を拾える代わりに、ロックされた vault で再起動ループが起きる。#754 以降はそもそも backend に触らない（Decision 3）
- **lock の競合も 4 にまとめる** — 再起動しないという扱いは同じだが、アプリが「config を直せ」と誤った案内を出す（Decision 4）
- **sysexits.h の値（78 / 75）** — Decision 7
- **エンベロープに `kind` などの分類フィールドを足す** — exit code と同じ情報を 2 箇所で持つことになる

# Consequences

- 監視する側は exit code だけで「再起動するか」を決められる。理由は `run --json` の stderr エンベロープから取り出せる
- config の誤りで exit 1 を前提にしていたスクリプトは 4 を受け取る。ADR-0012 は 1 を「より特定の code を持たない全エラー」と定義しており、一部を特定の code に切り出すのはその契約の範囲内なので、破壊的変更として扱わない
- launchd の `KeepAlive` は成功終了かどうかしか見ないので、この区別を活かせるのはアプリが自分で子プロセスを監視する構成（#754）に限られる
- **既知の制約**: config の検証エラーでは、エンベロープの前に `config error: …` の平文の行が検証結果の数だけ stderr に出る（この ADR 以前からの挙動）。`--json` の呼び出し側は stderr の**最後の行**をエンベロープとして読む必要があり、個々の検証結果はエンベロープに載らない（`message` は「configuration is invalid」）
- #754 の `secret:` スキームの解決失敗も、Decision 2 の「機密参照の解決失敗 = 4」にそのまま乗る
- 起動工程に新しい呼び出しを足すときは、その失敗が「人が直すまで直らない」かを判断して `needs_fix` で包むかを決める必要がある。包み忘れると 1 になる（安全側: 監視は再起動を試みる）
- 回帰テストは `crates/orchestrator-cli/tests/e2e.rs` の `run_exits_*` 群（壊れた config、無い `env_file`、解決できない機密参照、`CONFIG_INVALID`、無いプラグインバイナリ、プロトコル版の合わないプラグイン、lock の競合、対照として state DB が開けないときの 1）
