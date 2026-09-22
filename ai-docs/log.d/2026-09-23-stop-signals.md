* **Update**: [運用ガイド](/operations/operations-guide.md) — `run --watch` が SIGINT だけでなく SIGTERM / SIGHUP でも graceful 停止するようにした（#753）。launchd・`brew services`・`kill` の SIGTERM や端末を閉じたときの SIGHUP で即死し、`health.json` などが残っていた。修正前後の実測（プラグインは親の死で stdin EOF を受けて抜けるので孤児にならない）と、固まった git による停止遅延の注意も載せた
* **Update**: [orchestrator-spec](/product/orchestrator-spec.md) / [ja](/product/orchestrator-spec.ja.md) — F-74 に停止シグナル 3 種を明記した
* **Update**: [orchestrator-cli](/components/orchestrator-cli.md) / [リリースチェックリスト](/quality/release-checklist.md) — `run` の停止シグナルと、起動中に届いた停止では初回 dispatch をしないことを反映した
