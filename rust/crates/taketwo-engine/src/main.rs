// Skeleton (step S01): answers --version, and `seed` (step S04). The real program arrives in later steps of rust/PLAN.md.
mod seed;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("taketwo-engine {}", env!("CARGO_PKG_VERSION"));
            println!("{}", taketwo_voice::link_report());
        }
        Some("seed") => {
            if let Err(e) = seed::run(&args[1..]) {
                eprintln!("taketwo-engine seed: {e}");
                std::process::exit(2);
            }
        }
        _ => {
            eprintln!("taketwo-engine: not built yet; only --version and seed work in this skeleton");
            std::process::exit(2);
        }
    }
}
