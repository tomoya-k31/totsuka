-- spec §8.1 / §11.2: イベント単位デデュープ。週パーティションで保持期間後に DROP
CREATE TABLE IF NOT EXISTS processed_events (
  event_key     TEXT         NOT NULL,
  source        TEXT         NOT NULL,
  event_type    TEXT         NOT NULL,
  payload_hash  TEXT,
  received_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
  PRIMARY KEY (event_key, received_at)
) PARTITION BY RANGE (received_at);

-- 初期パーティション: 当週 + 翌週 (orchestrator nightly job が以降を作る)
-- 日付計算は手動 (date_trunc('week', now()) を即値化するため migration 適用時に
-- 計算する。実本番では orchestrator がパーティションを先読みで作る)
DO $$
DECLARE
    wk_start DATE := date_trunc('week', now())::date;
    wk_end   DATE;
    pname    TEXT;
BEGIN
    FOR i IN 0..1 LOOP
        wk_end := wk_start + INTERVAL '7 days';
        pname  := format('processed_events_%s', to_char(wk_start, 'YYYYMMDD'));
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF processed_events
             FOR VALUES FROM (%L) TO (%L)',
            pname, wk_start, wk_end
        );
        wk_start := wk_end;
    END LOOP;
END $$;

-- 検索用
CREATE INDEX IF NOT EXISTS idx_processed_events_source_received
  ON processed_events (source, received_at DESC);

INSERT INTO schema_meta (version) VALUES (1) ON CONFLICT DO NOTHING;
