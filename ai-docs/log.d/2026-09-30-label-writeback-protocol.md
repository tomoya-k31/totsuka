* **Decision**: [ADR-0108 on_* でラベルを付け外しする](/decisions/adr-0108-label-writeback.md) — `labels = ["+a", "-b"]`、新メソッド `task/update_labels` と capability `label_writeback`、閉路検査を拡張しない理由
* **Update**: [plugin-protocol](/components/plugin-protocol.md) — 0.7.6 で `task/update_labels`・`Capabilities.label_writeback`・`WorkflowInfo.label_writebacks`
* **Update**: [plugin-sdk](/components/plugin-sdk.md) / [プラグイン開発ガイド](/development/plugin-dev-guide.md) — `TaskSourceHandler::update_labels`（既定 `METHOD_NOT_FOUND`、`label_writeback` でゲート）
