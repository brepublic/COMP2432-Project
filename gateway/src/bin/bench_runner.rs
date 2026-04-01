#[path = "../benchmark/mod.rs"]
mod benchmark;

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
