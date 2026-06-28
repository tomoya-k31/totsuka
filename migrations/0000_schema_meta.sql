-- pgmq 拡張 (image 内蔵だがアプリ DB に明示インストール)
CREATE EXTENSION IF NOT EXISTS pgmq;

-- bin↔DB ハンドシェイク用 (spec §11.1)
-- version は最新の migration 番号と一致させる。bin は MIN/TARGET_SCHEMA_VERSION でこれを照合
CREATE TABLE IF NOT EXISTS schema_meta (
  version     INT          PRIMARY KEY,
  applied_at  TIMESTAMPTZ  NOT NULL DEFAULT now()
);

-- 本 migration 自身を記録
INSERT INTO schema_meta (version) VALUES (0) ON CONFLICT DO NOTHING;
