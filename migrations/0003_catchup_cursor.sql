-- spec §9 / parent §13.2: 発生源/スコープ単位のカーソル
CREATE TABLE IF NOT EXISTS catchup_cursor (
  source      TEXT         NOT NULL,
  scope       TEXT         NOT NULL,
  cursor      TEXT         NOT NULL,
  updated_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
  PRIMARY KEY (source, scope)
);

INSERT INTO schema_meta (version) VALUES (3) ON CONFLICT DO NOTHING;
