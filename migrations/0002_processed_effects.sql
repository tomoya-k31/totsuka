-- spec §8.2 / §13.1 / §11.15: 副作用の実行権 (lease 付き)
-- effect_key = "spawn:{task_id}:{phase}:{attempt}" 形式 (DiffBack で attempt+1)
CREATE TABLE IF NOT EXISTS processed_effects (
  effect_key        TEXT         NOT NULL,
  event_key         TEXT         NOT NULL,
  effect_type       TEXT         NOT NULL,
  status            TEXT         NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','in_progress','done','failed')),
  lease_owner       TEXT,
  lease_expires_at  TIMESTAMPTZ,
  attempts          INT          NOT NULL DEFAULT 0,
  result            JSONB,
  created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
  updated_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
  PRIMARY KEY (effect_key, created_at)
) PARTITION BY RANGE (created_at);

DO $$
DECLARE
    wk_start DATE := date_trunc('week', now())::date;
    wk_end   DATE;
    pname    TEXT;
BEGIN
    FOR i IN 0..1 LOOP
        wk_end := wk_start + INTERVAL '7 days';
        pname  := format('processed_effects_%s', to_char(wk_start, 'YYYYMMDD'));
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF processed_effects
             FOR VALUES FROM (%L) TO (%L)',
            pname, wk_start, wk_end
        );
        wk_start := wk_end;
    END LOOP;
END $$;

-- 回収 (期限切れ in_progress / failed retryable)
CREATE INDEX IF NOT EXISTS idx_effects_recoverable
  ON processed_effects (status, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_effects_event_key
  ON processed_effects (event_key);

INSERT INTO schema_meta (version) VALUES (2) ON CONFLICT DO NOTHING;
