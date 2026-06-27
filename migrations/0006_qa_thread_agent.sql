-- spec §8.4: Slack thread_ts → herdr terminal_id mapping
CREATE TABLE IF NOT EXISTS qa_thread_agent (
  thread_ts         TEXT         PRIMARY KEY,
  terminal_id       TEXT         NOT NULL,
  repo              TEXT         NOT NULL,
  last_activity_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
  created_at        TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_qa_thread_agent_last_activity
  ON qa_thread_agent (last_activity_at);

INSERT INTO schema_meta (version) VALUES (6) ON CONFLICT DO NOTHING;
