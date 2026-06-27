-- spec §11.4: status は ColumnId snake_case で保存 (totsuka-core の serde 形式と一致)
CREATE TABLE IF NOT EXISTS gh_item_status (
  item_id      TEXT         PRIMARY KEY,            -- ProjectV2Item.id (PVTI_...)
  status       TEXT,                                  -- ColumnId snake_case or NULL
  content_ref  TEXT,                                  -- "owner/repo#123" 観測用
  closed_at    TIMESTAMPTZ,                           -- item close 検知時刻 (retention 用)
  updated_at   TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_gh_item_status_closed
  ON gh_item_status (closed_at) WHERE closed_at IS NOT NULL;

INSERT INTO schema_meta (version) VALUES (4) ON CONFLICT DO NOTHING;
