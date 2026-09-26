* **Creation**: [ADR-0099](/decisions/adr-0099-generated-hook-token.md) — hook の Bearer トークンは `totsuka run` が生成して `$XDG_STATE_HOME/totsuka/hook-token`（0600）に保存し、以後は使い回す（#785）。`[hooks].auth_token_ref` と `TOTSUKA_HOOKS_AUTH_TOKEN_REF` は猶予なしで廃止し、書いてあれば「この行を消す」専用のエラーにする
* **Update**: [hook-security](/security/hook-security.md) — §1 のトークンの供給と保管、ローテーション、`hook-token` / `hook-socket` チェックを生成ファイル方式に書き換えた
* **Update**: [config-reference](/development/config-reference.md) — `[hooks]` から `auth_token_ref` を削除し、トークンの仕組みと移行手順（行の削除・Keychain 項目の消し方）を追記
* **Update**: [hook-troubleshooting](/operations/hook-troubleshooting.md) — `hook-token` / `hook-socket` の読み方、廃止エラーへの対処、ローテーション手順
* **Update**: [ADR-0004](/decisions/adr-0004-hook-completion-signal.md) / [ADR-0094](/decisions/adr-0094-task-control-endpoints.md) — トークンの出どころが ADR-0099 で変わったことを相互参照
