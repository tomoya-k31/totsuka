# 各スクリプトと `.env` の両方から source される共通定義。
#
# `tt` をここに置いているのは、シェル関数が子プロセスへ継承されないため。
# `.env` の中だけで定義すると、`bash scripts/github.sh` のように別プロセスで
# 起動したスクリプトからは `tt: command not found` になる。定義を 1 つにして
# 両方から source すれば、人間の対話シェルでもスクリプト内でも同じものが使える。
#
# 何度 source しても安全（再定義するだけ）。

: "${E2E_HOME:?E2E_HOME が未設定です。リポジトリルートで source .env してください}"
: "${E2E_TOTSUKA_BIN:?E2E_TOTSUKA_BIN が未設定です}"

# XDG は export しない。gh が $XDG_CONFIG_HOME/gh を読むため、export すると
# シェル全体の GitHub 認証が壊れる。env で totsuka の起動時にだけ被せる。
tt() {
  env E2E_HOME="$E2E_HOME" \
      E2E_GH_TOKEN="${E2E_GH_TOKEN:-}" \
      E2E_HOOK_TOKEN="${E2E_HOOK_TOKEN:-}" \
      XDG_CONFIG_HOME="$E2E_HOME/cfg" \
      XDG_DATA_HOME="$E2E_HOME/data" \
      XDG_STATE_HOME="$E2E_HOME/state" \
      XDG_CACHE_HOME="$E2E_HOME/cache" \
      NO_COLOR=1 \
      "$E2E_TOTSUKA_BIN" "$@"
}
