//! `taketwo-engine --version | seed | serve` (rust/PLAN.md steps S01, S04, S06).
use taketwo_engine::{seed, serve};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let result = match args.first().map(String::as_str) {
        Some("--version") => {
            println!("taketwo-engine {}", env!("CARGO_PKG_VERSION"));
            println!("{}", taketwo_voice::link_report());
            Ok(())
        }
        Some("seed") => seed::run(rest),
        Some("serve") => serve::run(rest),
        _ => Err("usage: taketwo-engine --version | seed [--fixture FILE] | serve [--port N] [--ui DIR]".into()),
    };
    if let Err(e) = result {
        eprintln!("taketwo-engine {}: {e}", args.first().map(String::as_str).unwrap_or(""));
        std::process::exit(2);
    }
}
