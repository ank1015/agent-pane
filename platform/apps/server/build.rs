fn main() {
    // sqlx::migrate! embeds migration files. New files must invalidate a cached
    // build even when none of the existing Rust sources changed.
    println!("cargo:rerun-if-changed=migrations");
}
