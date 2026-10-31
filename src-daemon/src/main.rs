mod bmc_scraper;
mod cache;
mod lemonade;
mod mail_scraper;
mod mcp;
mod model_scraper;
mod llama_scraper;
mod scraper;
mod types;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use cache::TelemetryCache;
use clap::Parser;
use mcp::{JsonRpcRequest, McpHandler};
use scraper::HardwareScraper;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, Level};
use types::DaemonHealth;

#[derive(Parser, Debug)]
#[command(name = "nauti-daemon", author = "Omarchy / Cesarops Team", version = "0.1.0", about = "Lean agent-first hardware & Lemonade telemetry daemon with MCP surface")]
struct Args {
    /// Bind address for HTTP service
    #[arg(long, env = "NAUTI_BIND", default_value = "127.0.0.1:9100")]
    bind: String,

    /// Lemonade LLM daemon base URL
    #[arg(long, env = "LEMONADE_URL", default_value = "http://127.0.0.1:13305")]
    lemonade_url: String,

    /// Node identifier in telemetry payload
    #[arg(long, env = "NAUTI_NODE_NAME", default_value = "cesarops-3")]
    node_name: String,

    /// Telemetry scrape interval in seconds
    #[arg(long, env = "NAUTI_SCRAPE_INTERVAL", default_value = "1")]
    scrape_interval: u64,

    /// Ring buffer capacity for historical telemetry samples
    #[arg(long, env = "NAUTI_HISTORY_CAPACITY", default_value = "120")]
    history_capacity: usize,

    /// Run in MCP stdio mode for direct agent connection
    #[arg(long)]
    mcp_stdio: bool,
}

struct AppState {
    cache: Arc<TelemetryCache>,
    mcp: Arc<McpHandler>,
    start_time: Instant,
    sysinfo_ok: Arc<AtomicBool>,
    all_smi_ok: Arc<AtomicBool>,
    polaris_fallback_active: Arc<AtomicBool>,
    lemonade_reachable: Arc<AtomicBool>,
}

#[derive(Deserialize)]
struct HistoryQuery {
    seconds: Option<u64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    let args = Args::parse();
    info!("Starting Nauti-Monitor daemon for node '{}'", args.node_name);

    let cache = Arc::new(TelemetryCache::new(args.history_capacity));
    let mcp = Arc::new(McpHandler::new(cache.clone()));

    let sysinfo_ok = Arc::new(AtomicBool::new(true));
    let all_smi_ok = Arc::new(AtomicBool::new(false));
    let polaris_fallback_active = Arc::new(AtomicBool::new(false));
    let lemonade_reachable = Arc::new(AtomicBool::new(false));

    // Immediate initial scrape so cache is hot at t=0
    let mut scraper = HardwareScraper::new(args.node_name.clone(), args.lemonade_url.clone(), None, None);
    let initial_sample = scraper.scrape().await;
    all_smi_ok.store(scraper.all_smi_ok, Ordering::Relaxed);
    polaris_fallback_active.store(scraper.polaris_fallback_active, Ordering::Relaxed);
    lemonade_reachable.store(initial_sample.lemonade.healthy, Ordering::Relaxed);
    cache.update(initial_sample).await;

    // Stdio MCP mode shortcut
    if args.mcp_stdio {
        info!("Running in MCP stdio-only mode");
        let cache_clone = cache.clone();
        tokio::spawn(async move {
            run_scraper_loop(
                scraper,
                cache_clone,
                args.scrape_interval,
                Some(sysinfo_ok),
                Some(all_smi_ok),
                Some(polaris_fallback_active),
                Some(lemonade_reachable),
            )
            .await;
        });
        mcp.run_stdio_loop().await;
        return Ok(());
    }

    // Spawn background scraper task
    let cache_scraper = cache.clone();
    let sysinfo_flag = sysinfo_ok.clone();
    let all_smi_flag = all_smi_ok.clone();
    let polaris_flag = polaris_fallback_active.clone();
    let lemonade_flag = lemonade_reachable.clone();

    tokio::spawn(async move {
        run_scraper_loop(
            scraper,
            cache_scraper,
            args.scrape_interval,
            Some(sysinfo_flag),
            Some(all_smi_flag),
            Some(polaris_flag),
            Some(lemonade_flag),
        )
        .await;
    });

    let app_state = Arc::new(AppState {
        cache: cache.clone(),
        mcp: mcp.clone(),
        start_time: Instant::now(),
        sysinfo_ok,
        all_smi_ok,
        polaris_fallback_active,
        lemonade_reachable,
    });

    // Build Axum HTTP routes
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/telemetry", get(telemetry_handler))
        .route("/api/telemetry", get(telemetry_handler))
        .route("/history", get(history_handler))
        .route("/api/telemetry/history", get(history_handler))
        .route("/mcp", post(mcp_http_handler))
        .layer(cors)
        .with_state(app_state);

    let addr: SocketAddr = args.bind.parse()?;
    info!("Nauti-Monitor HTTP & MCP server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn run_scraper_loop(
    mut scraper: HardwareScraper,
    cache: Arc<TelemetryCache>,
    interval_secs: u64,
    sysinfo_flag: Option<Arc<AtomicBool>>,
    all_smi_flag: Option<Arc<AtomicBool>>,
    polaris_flag: Option<Arc<AtomicBool>>,
    lemonade_flag: Option<Arc<AtomicBool>>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));

    loop {
        interval.tick().await;
        let sample = scraper.scrape().await;

        if let Some(ref flag) = sysinfo_flag {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(ref flag) = all_smi_flag {
            flag.store(scraper.all_smi_ok, Ordering::Relaxed);
        }
        if let Some(ref flag) = polaris_flag {
            flag.store(scraper.polaris_fallback_active, Ordering::Relaxed);
        }
        if let Some(ref flag) = lemonade_flag {
            flag.store(sample.lemonade.healthy, Ordering::Relaxed);
        }

        cache.update(sample).await;
    }
}

async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let uptime = state.start_time.elapsed().as_secs();
    let snap = state.cache.snapshot().await;
    let mail_reachable = snap.mail_health.smtp_online;
    let bmc_detected = snap.bmc_health.as_ref().map(|b| b.bmc_detected).unwrap_or(false);

    let health = DaemonHealth {
        status: "ok".to_string(),
        uptime_secs: uptime,
        sysinfo_ok: state.sysinfo_ok.load(Ordering::Relaxed),
        all_smi_ok: state.all_smi_ok.load(Ordering::Relaxed),
        polaris_fallback_active: state.polaris_fallback_active.load(Ordering::Relaxed),
        lemonade_reachable: state.lemonade_reachable.load(Ordering::Relaxed),
        mail_reachable,
        bmc_detected,
    };
    (StatusCode::OK, Json(health))
}

async fn telemetry_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let snap = state.cache.snapshot().await;
    (StatusCode::OK, Json(snap))
}

async fn history_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HistoryQuery>,
) -> impl IntoResponse {
    let hist = state.cache.history(query.seconds).await;
    (StatusCode::OK, Json(hist))
}

async fn mcp_http_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let resp = state.mcp.handle_request(req).await;
    (StatusCode::OK, Json(resp))
}
