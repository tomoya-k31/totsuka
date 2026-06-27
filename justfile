set shell := ["bash", "-cu"]

# pgmq コンテナ起動
pgmq-up:
    docker compose -f deploy/docker-compose.yml up -d pgmq

pgmq-down:
    docker compose -f deploy/docker-compose.yml down

pgmq-logs:
    docker compose -f deploy/docker-compose.yml logs -f pgmq

# migration 適用 (DATABASE_URL 必須)
db-migrate:
    sqlx migrate run --source migrations

db-info:
    sqlx migrate info --source migrations

# テスト用 DB 再生成
db-reset:
    psql "$DATABASE_URL" -c "DROP DATABASE IF EXISTS totsuka_test"
    psql "$DATABASE_URL" -c "CREATE DATABASE totsuka_test"

# workspace 全テスト
test:
    cargo test --workspace --all-features

# lint
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo fmt --check
