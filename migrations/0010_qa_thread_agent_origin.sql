-- スレッドの由来。'owner' = 従来の bot メンション/継続フロー、
-- 'self_mention' = 同僚→owner メンションのカンペ回答フロー。
-- self_mention 由来スレッドは owner の素の返信では継続発火させない
-- (default_mode=auto での公開リーク・dm_only 状態の持ち越しを防ぐ)。
ALTER TABLE qa_thread_agent
  ADD COLUMN IF NOT EXISTS origin TEXT NOT NULL DEFAULT 'owner';

INSERT INTO schema_meta (version) VALUES (10) ON CONFLICT DO NOTHING;
