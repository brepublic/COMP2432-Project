//! Shared sensor rate parsing (must match gateway `main` registration logic).

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

pub fn rate_for_index(rates: &[u32], idx: usize) -> u32 {
    if let Some(rate) = rates.get(idx) {
        *rate
    } else {
        *rates.last().unwrap_or(&1)
    }
}
