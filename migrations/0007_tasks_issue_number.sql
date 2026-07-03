-- The GitHub issue number behind a project item. Carried on
-- github.status_changed events by the watcher and used to build the
-- prompt handed to a spawned agent ("work on {repo}#{issue_number}").
-- Nullable: draft project items have no linked content.
ALTER TABLE tasks ADD COLUMN IF NOT EXISTS issue_number BIGINT;

INSERT INTO schema_meta (version) VALUES (7) ON CONFLICT DO NOTHING;
