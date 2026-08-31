* **Fix**: `doctor` の `hook-socket` が **stale socket を「a receiver is live」と報告していた**。`is_socket` は `symlink_metadata` でファイル種別を見るだけで、listener が居るかは一切見ていない。connect だけを行うプローブを `is_socket` の直後に置き、live と stale を実際に区別するようにした。

* **Note**: **バグが表に出るのはトークンのスキームを変えた瞬間だった。** 自己 POST（`self_post`）はこれまで唯一 live を証明していた経路で、失敗すれば `Connection refused` を掴んで正確に「stale socket かもしれない、消して再起動を」と案内できていた。ところが `[hooks].auth_token_ref` を `keychain:` から `op://` へ変えると、**op セッションが無いときに自己 POST の手前で `skip` して return する**ため、検証していない「live」だけが出力に残る。`cmd:` にも同じ早期 return があるので、**同じ穴が 2 つあった**。

* **Note**: **早期 return を足すときは、その手前で既に断定していることを見直す。** コードは `if !is_socket { ... return; }` の直後にコメントで `// A receiver is live:` と書いており、当時はその直後が自己 POST だったので正しかった。後から `op://` / `cmd:` のゲートを**その間に**挿したときに、断定だけが取り残された。**挿入した本人にとっては「早期 return を足した」だけで、主張を弱めたつもりが無い。**

* **Note**: 実害は「stale socket を消す案内が消える」こと。旧経路の action は `remove the stale socket file and restart` を持っていたが、新しい skip 経路は `op signin` を勧めるだけで、**そこに受信側が居ると信じたまま調査を始めることになる**。

* **Note**: **変異でテストを検証した。** `can_connect` を「常に成功」へ変えるとテストが落ち、失敗メッセージに実機と同一の文言（`a receiver is live at … but resolving its op:// reference would prompt`）が再現した。なお**プローブの呼び出しごと消す変異は使えない** —— `can_connect` が dead code になり、`[workspace.lints.rust] warnings = "deny"` でビルドが先に落ちるので、テストの良し悪しを測れない。
