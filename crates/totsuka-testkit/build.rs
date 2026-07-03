fn main() {
    // sqlx::migrate! embeds the migration files at compile time, but adding a
    // NEW file does not invalidate the build on its own — ephemeral test DBs
    // would silently miss the latest migration. Recompile whenever the
    // migrations directory changes.
    println!("cargo:rerun-if-changed=../../migrations");
}
