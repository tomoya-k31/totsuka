* **Creation**: [ADR-0114](/decisions/adr-0114-macos-app-gh-token.md) — メニューバーアプリが `[github].token` の `secret:` を、入力ダイアログの「Use gh auth token」で選べば Start のたびに `gh auth token --hostname <host> --user <github_login>` から取る（保存しない）。PAT の発行を不要にする（#858）
* **Update**: [macOS アプリ](/components/macos-app.md) — 起動の流れ、`Launch` / `CLI` の関数、Forget の挙動を追記
