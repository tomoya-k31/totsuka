* **Creation**: [ADR-0109 ネイティブ macOS メニューバーアプリが run を子プロセスとして監督し、通知と設定 GUI を持つ](/decisions/adr-0109-native-menubar-app.md) — SwiftUI のメニューバーアプリ（apps/macos/）の技術選定・監督方式・通知経路・設定 GUI・配布方式と、プロトタイプでの実測（Keychain の partition_id、/tmp の通知拒否、ログイン項目の追従）を記録。
* **Update**: [plugin-protocol クレート](/components/plugin-protocol.md) — プロトコル 0.7.7。initialize 前に答える `config/schema` と、それを申告する `Capabilities.config_schema` を追加。
* **Update**: [plugin-sdk クレート](/components/plugin-sdk.md) / [plugin-conformance](/components/plugin-conformance.md) / [プラグイン開発ガイド](/development/plugin-dev-guide.md) — `config_schema` ハンドラの既定 METHOD_NOT_FOUND、適合検査 10、スキーマの拡張キーワード（`x-title` / `x-help` / `x-category` / `x-secret`）。
* **Update**: [ADR-0065](/decisions/adr-0065-menubar-status.md) — 「Swift アプリ」却下の行を ADR-0109 が覆したことを Status に追記。
* **Update**: [Orchestrator 仕様](/product/orchestrator-spec.md) — §3.2 の常駐デーモンの行をメニューバーアプリの子プロセス起動に合わせて改訂し、メソッド表に `config/schema` を追加。
