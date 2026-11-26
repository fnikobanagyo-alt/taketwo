// Skeleton (step S01): answers --version only. The real program arrives in later steps of rust/PLAN.md.
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("taketwo-engine {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    eprintln!("taketwo-engine: not built yet; only --version works in this skeleton");
    std::process::exit(2);
}
