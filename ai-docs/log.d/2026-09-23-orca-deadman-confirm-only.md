* **Update**: [agent-ide-orca](/components/agent-ide-orca.md) — deadman が `failed` を送るのは `terminal show` で端末の終了を確かめたときだけにした（#768）。`terminal wait` の失敗が 5 回続いたら確かめずに `failed` にする打ち切りと、スリープをまたいだら数え直す `ErrorRun` を削除した。待ち直しの間隔は 2 秒から倍々で最長 60 秒
* **Update**: [運用ガイド](/operations/operations-guide.md) — 「スリープ明けに複数タスクがまとめて failed」の FAQ を、0.8.5 の直し方に合わせて書き直した
