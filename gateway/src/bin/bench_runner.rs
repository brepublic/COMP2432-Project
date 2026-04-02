#[path = "../benchmark/mod.rs"]
mod benchmark;
#[path = "../rates.rs"]
mod rates;

/// CLI entry: `bench_runner [path/to/benchmark.toml]` (defaults to `benchmark.toml`).
///
/// # Returns
///
/// Exits with code `1` if [`benchmark::run`] fails.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let config_path = if args.len() > 1 {
        args[1].as_str()
    } else {
        "benchmark.toml"
    };
    let path = std::path::Path::new(config_path);
    if let Err(e) = benchmark::run(path) {
        eprintln!("benchmark failed: {e}");
        std::process::exit(1);
    }
}
