//! Shared sensor rate parsing (must match gateway `main` registration logic).

/// Parses a whitespace-separated list of positive per-sensor rates.
///
/// # Arguments
///
/// * `spec` — Space-separated `u32` tokens (e.g. `"50 30"`).
/// * `default_rate` — Used when `spec` is empty or yields no valid positive integers.
///
/// # Returns
///
/// A non-empty `Vec<u32>` of rates (at least `[default_rate]` when parsing fails).
pub fn parse_rates(spec: &str, default_rate: u32) -> Vec<u32> {
    let parsed: Vec<u32> = spec
        .split_whitespace()
        .filter_map(|token| token.parse::<u32>().ok())
        .filter(|rate| *rate > 0)
        .collect();
    if parsed.is_empty() {
        vec![default_rate]
    } else {
        parsed
    }
}

/// Selects the rate for sensor index `idx`, reusing the last entry for out-of-range indices.
///
/// # Arguments
///
/// * `rates` — Non-empty slice from [`parse_rates`] (or equivalent).
/// * `idx` — Zero-based sensor index within its family.
///
/// # Returns
///
/// `rates[idx]` if present, otherwise `*rates.last().unwrap_or(&1)`.
pub fn rate_for_index(rates: &[u32], idx: usize) -> u32 {
    if let Some(rate) = rates.get(idx) {
        *rate
    } else {
        *rates.last().unwrap_or(&1)
    }
}
