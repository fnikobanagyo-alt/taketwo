//! `taketwo-mcp [--name taketwo|voiceforge]` over stdio, isolated mode only (`TAKETWO_DATA`); `--version`.
use rmcp::ServiceExt;
use std::sync::Arc;
use taketwo_core::paths::Mode;
use taketwo_mcp::engine::{self, Finder};
use taketwo_mcp::http::{Transport, Ureq};
use taketwo_mcp::server::Server;
use taketwo_mcp::tools::Tools;

fn setup(args: &[String]) -> Result<Server, String> {
    let mut name = "taketwo".to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => match it.next().map(String::as_str) {
                Some(n @ ("taketwo" | "voiceforge")) => name = n.into(),
                other => return Err(format!("--name must be taketwo or voiceforge, got {other:?}")),
            },
            "--live" => {}
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let data = match Mode::from_env_and_args(args)? {
        Mode::Isolated { data } => data,
        Mode::Live => return Err("--live is not built yet; set TAKETWO_DATA to a temporary folder".into()),
    };
    let pinned = engine::pinned(std::env::var("TAKETWO_URL").ok(), std::env::var("VOICEFORGE_URL").ok());
    if let Some(p) = &pinned {
        engine::check_pinned(p)?;
    }
    let http: Arc<dyn Transport> = Arc::new(Ureq);
    let finder = Arc::new(Finder::new(http.clone(), data, pinned, engine::engine_program()));
    let downloads = std::env::home_dir().unwrap_or_default().join("Downloads").join("TakeTwo");
    let tools = Tools { http, engine: Arc::new(move || finder.base()), downloads };
    Ok(Server { name, tools: Arc::new(tools) })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("taketwo-mcp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let server = setup(&args).unwrap_or_else(|e| {
        eprintln!("taketwo-mcp: {e}");
        std::process::exit(2);
    });
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    let served = rt.block_on(async {
        let running = server.serve(rmcp::transport::stdio()).await.map_err(|e| e.to_string())?;
        running.waiting().await.map(|_| ()).map_err(|e| e.to_string())
    });
    if let Err(e) = served {
        eprintln!("taketwo-mcp: {e}");
        std::process::exit(1);
    }
}
