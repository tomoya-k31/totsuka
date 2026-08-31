* **Update**: リポジトリを **public へ切り替えた**（#506）。[release-runbook](/operations/release-runbook.md) のトークン表に `HOMEBREW_TAP_TOKEN` の失効日（2026-09-30）を記録し、可視性ゲートの節を「public 化までステップを止めている」から「発火済み」へ書き換えた。

* **Note**: **`pull_request_creation_policy` は可視性変更を跨いで保たれた。** #506 の中心的な未確認事項で、`collaborators_only` が既定へ戻る可能性を疑っていたが、実測では戻らず再適用は不要だった。`allow_forking` は public では変更できないが、`collaborators_only` がある以上 fork されても PR は作れないので目的は満たされている。

* **Note**: **失効時に落ちるのは `git clone` ではなく `git push` だった。** runbook はこれまで clone が落ちると書いていたが、tap 自体が public なので**トークンが空でも clone は成功する**（実測: `git ls-remote 'https://x-access-token:@github.com/tomoya-k31/homebrew-tap.git'` は rc=0）。`sed` も直後の `grep -q` の表明も通り、最後の push で初めて認証が要る。**赤くなった run の原因を clone のログに探しても何も無い**ので、誤誘導としては安くない。

* **Note**: **可視性ゲートの副作用が 1 度出た。** bump ステップは `if: ${{ !github.event.repository.private }}` でゲートしてあるので、private 中に出した v0.6.0（2026-08-29）ではスキップされ、formula が v0.5.0 に取り残された。**v0.5.0 のアセットは実在するので `brew install` は成功したうえで古いものを入れる** —— 壊れずに古くなる方向の失敗なので、誰も気づかない。public 化に合わせて手で合わせた。**private のままリリースを重ねるとそのぶん静かに開く差**である。

* **Note**: **可視性ゲートは「public 化した瞬間に自分で有効になる」ので、トークンの登録が切り替えより後だとリリースが赤くなる。** 設計どおりだが手順としては先後があり、`HOMEBREW_TAP_TOKEN` の登録を切り替えより前に置くのが正しい。検証も `git ls-remote` では**できない**（上記のとおり空トークンでも通る）ので、`gh api repos/tomoya-k31/homebrew-tap --jq .permissions` の `push` を見るか、実際に push する。

* **Note**: public 化で無料になった **secret scanning と push protection を有効化した**。PR #505 の監査と 2026-08-31 の再監査はどちらも手作業の走査だったので、履歴全体を対象にした独立な機械検査で裏が取れた（アラート **0 件**、8 分追跡）。`dependabot_security_updates` は [audit.yml](https://github.com/tomoya-k31/totsuka/blob/main/.github/workflows/audit.yml) の日次 `cargo audit` / `cargo deny` と目的が重なるので入れていない。
