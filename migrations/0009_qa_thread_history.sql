-- QA 会話履歴の永続化。delegated モードの回答はエフェメラルで Slack 側に
-- 残らないため、pane close 後の再 spawn で会話文脈を復元する唯一の情報源。
CREATE TABLE IF NOT EXISTS qa_thread_history (
  id          BIGSERIAL    PRIMARY KEY,
  thread_ts   TEXT         NOT NULL,
  role        TEXT         NOT NULL,  -- 'user' | 'assistant'
  body        TEXT         NOT NULL,
  created_at  TIMESTAMPTZ  NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_qa_thread_history_thread
  ON qa_thread_history (thread_ts, id);

INSERT INTO schema_meta (version) VALUES (9) ON CONFLICT DO NOTHING;
