* **Creation**: [Totsuka.app（macOS メニューバーアプリ）](/components/macos-app.md) — `apps/macos/` の SwiftUI アプリ。run の監督・通知・スキーマから組み立てる設定画面。
* **Update**: [ADR-0109](/decisions/adr-0109-native-menubar-app.md) — タスク操作は CLI 経由、SwiftPM と XcodeGen の分担、アプリの文言の選び方。
* **Update**: [release runbook](/operations/release-runbook.md) / [Homebrew tap](/infrastructure/homebrew-tap.md) — tarball に `Totsuka.app` が入り、formula 側で入れる変更（tap リポジトリで行う）。
* **Update**: [テスト戦略](/quality/test-strategy.md) — CI の `macos app` ジョブ。
