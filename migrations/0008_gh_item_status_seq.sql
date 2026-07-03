-- Per-item status transition generation. Incremented every time the
-- status actually changes; baked into gh:status event keys so that
-- moving a card into the same column a second time (design-review
-- sending work back to design) is a NEW event instead of being absorbed
-- by processed_events forever. Redeliveries of the same event share the
-- same seq, so consumer idempotency is unchanged.
ALTER TABLE gh_item_status ADD COLUMN IF NOT EXISTS status_seq BIGINT NOT NULL DEFAULT 0;

INSERT INTO schema_meta (version) VALUES (8) ON CONFLICT DO NOTHING;
