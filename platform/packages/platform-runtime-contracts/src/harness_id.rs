/// Shared harness-ID grammar: `[a-z][a-z0-9_-]{0,127}` (ASCII).
///
/// Validates spelling only, not catalogue existence or enabled state. Keep the
/// database constraint as defence in depth; all Rust callers use this function.
pub fn is_valid_harness_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
