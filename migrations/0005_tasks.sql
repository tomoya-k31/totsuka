-- spec §11.14: task_id = ProjectV2Item.id、task_id_short は末尾 12 文字
CREATE TABLE IF NOT EXISTS tasks (
  id                                  TEXT         PRIMARY KEY,
  task_id_short                       TEXT         NOT NULL UNIQUE,
  repo                                TEXT         NOT NULL,
  pr_node_id                          TEXT,
  current_column                      TEXT         NOT NULL,
  current_phase                       TEXT,
  impl_verify_attempt                 INT          NOT NULL DEFAULT 0,
  suppress_writeback_until_human_move BOOLEAN      NOT NULL DEFAULT FALSE,
  spawned_at                          TIMESTAMPTZ,
  created_at                          TIMESTAMPTZ  NOT NULL DEFAULT now(),
  updated_at                          TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_tasks_repo            ON tasks (repo);
CREATE INDEX IF NOT EXISTS idx_tasks_task_id_short   ON tasks (task_id_short);
CREATE INDEX IF NOT EXISTS idx_tasks_pr_node_id      ON tasks (pr_node_id)
  WHERE pr_node_id IS NOT NULL;

INSERT INTO schema_meta (version) VALUES (5) ON CONFLICT DO NOTHING;
