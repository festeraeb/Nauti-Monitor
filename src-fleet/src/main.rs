//! nauti-fleet — fleet-wide telemetry aggregator.
//!
//! Polls each node's `nauti-daemon` and exposes one canonical fleet view.
//! This is the ONE endpoint other programs should read.
//!
//!   GET  /api/fleet/telemetry — per-node snapshots + fleet rollup
//!   GET  /api/fleet/health    — per-node daemon health + reachability
//!   GET  /api/fleet/summary   — compact capacity/VRAM rollup (rescue path)
//!   GET  /api/fleet/nodes     — node inventory + reachability
//!   GET  /health              — aggregator liveness
//!   POST /mcp                 — JSON-RPC 2.0 MCP surface
//!
//! MCP tools: fleet_telemetry, fleet_health, fleet_summary, fleet_vram_leaks.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{error, info, warn};

/// Wire-compatible mirror of `UnifiedTelemetry`.
///
/// Declared locally rather than imported from the daemon crate: the workspace
/// resolves two axum majors (0.7 via tower-http, 0.8 via reqwest-adjacent
/// deps) and sharing the crate drags the conflicting `Handler` trait into this
/// bin. These are plain serde shapes with `#[serde(default)]` on every field,
/// so a node running a newer/older schema still deserializes instead of 500ing.
/// The daemon remains the single producer of this schema.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UnifiedTelemetry {
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub node_name: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub cpu_usage: f32,
    #[serde(default)]
    pub cpu_pct: f32,
    #[serde(default)]
    pub memory_used_bytes: u64,
    #[serde(default)]
    pub ram_used_bytes: u64,
    #[serde(default)]
    pub memory_total_bytes: u64,
    #[serde(default)]
    pub ram_total_bytes: u64,
    #[serde(default)]
    pub rx_bps: f64,
    #[serde(default)]
    pub tx_bps: f64,
    #[serde(default)]
    pub physical_nics: std::collections::HashMap<String, NicRate>,
    #[serde(default)]
    pub accelerators: Vec<GpuStats>,
    #[serde(default)]
    pub gpu_stats: Vec<GpuStats>,
    #[serde(default)]
    pub lemonade_healthy: bool,
    #[serde(default)]
    pub mail_health: serde_json::Value,
    #[serde(default)]
    pub bmc_health: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NicRate {
    #[serde(default)]
    pub rx_bps: u64,
    #[serde(default)]
    pub tx_bps: u64,
}

/// Model serving telemetry per GPU — mirrors the daemon's ModelServingInfo so
/// per-GPU model metadata and passive TPS survive fleet aggregation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelServingInfo {
    /// Name of the model currently loaded on this GPU (or primary model if multi-GPU)
    #[serde(default)]
    pub model_name: Option<String>,
    /// Engine/serve backend: "llama-server", "vllm", "tabbyapi", etc.
    #[serde(default)]
    pub engine: Option<String>,
    /// API endpoint where this model is served (e.g., "http://127.0.0.1:8103/v1")
    #[serde(default)]
    pub endpoint: Option<String>,
    /// VRAM allocated to this model (bytes)
    #[serde(default)]
    pub vram_allocated: Option<u64>,
    /// System RAM offloaded for this model (bytes) - for CPU offloading
    #[serde(default)]
    pub ram_offloaded: Option<u64>,
    /// Current tokens/sec (instantaneous, from /slots decoded-token deltas)
    #[serde(default)]
    pub tps_current: Option<f32>,
    /// Average tokens/sec over recent window
    #[serde(default)]
    pub tps_avg: Option<f32>,
    /// Best/peak tokens/sec observed
    #[serde(default)]
    pub tps_best: Option<f32>,
    /// Context length configured
    #[serde(default)]
    pub context_length: Option<u32>,
    /// Quantization / precision
    #[serde(default)]
    pub quantization: Option<String>,
    /// GPU index this model is primarily using (for multi-GPU models)
    #[serde(default)]
    pub primary_gpu_index: Option<u32>,
    /// All GPU indices this model spans
    #[serde(default)]
    pub gpu_indices: Option<Vec<u32>>,
    /// Last updated timestamp (unix ms)
    #[serde(default)]
    pub last_updated_ms: Option<u64>,
    /// Health/status of the model server
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GpuStats {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub utilization: f32,
    #[serde(default)]
    pub vram_used: u64,
    #[serde(default)]
    pub vram_total: u64,
    #[serde(default)]
    pub temp_celsius: Option<f32>,
    #[serde(default)]
    pub power_watts: Option<f32>,
    /// Model serving information for this GPU
    #[serde(default)]
    pub model: Option<ModelServingInfo>,
}

/// Per-node daemon health, as reported by `GET /health` on the node.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeHealth {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub uptime_secs: u64,
    #[serde(default)]
    pub sysinfo_ok: bool,
    #[serde(default)]
    pub all_smi_ok: bool,
    #[serde(default)]
    pub polaris_fallback_active: bool,
    #[serde(default)]
    pub lemonade_reachable: bool,
    #[serde(default)]
    pub mail_reachable: bool,
    #[serde(default)]
    pub bmc_detected: bool,
}

/// What the aggregator knows about one node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    pub name: String,
    pub url: String,
    pub reachable: bool,
    pub error: Option<String>,
    pub age_secs: u64,
    pub health: Option<NodeHealth>,
    /// Raw telemetry exactly as the node reported it.
    pub telemetry: Option<UnifiedTelemetry>,
}

/// Fleet rollup computed from the per-node snapshots.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FleetSummary {
    pub nodes_total: usize,
    pub nodes_reachable: usize,
    pub gpus_total: usize,
    /// Accelerators reporting nonzero VRAM in use.
    pub gpus_busy: usize,
    pub vram_total_bytes: u64,
    pub vram_used_bytes: u64,
    pub vram_free_bytes: u64,
    pub max_temp_c: Option<f32>,
}

impl FleetSummary {
    fn compute(nodes: &[NodeStatus]) -> Self {
        let mut s = FleetSummary {
            nodes_total: nodes.len(),
            ..Default::default()
        };
        for n in nodes {
            if n.reachable {
                s.nodes_reachable += 1;
            }
            let Some(t) = n.telemetry.as_ref() else { continue };
            for g in &t.accelerators {
                s.gpus_total += 1;
                s.vram_total_bytes = s.vram_total_bytes.saturating_add(g.vram_total);
                s.vram_used_bytes = s.vram_used_bytes.saturating_add(g.vram_used);
                if g.vram_used > 0 {
                    s.gpus_busy += 1;
                }
                if let Some(temp) = g.temp_celsius {
                    match s.max_temp_c {
                        Some(m) if m >= temp => {}
                        _ => s.max_temp_c = Some(temp),
                    }
                }
            }
        }
        s.vram_free_bytes = s.vram_total_bytes.saturating_sub(s.vram_used_bytes);
        s
    }
}

/// Cached fleet state, refreshed on an interval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetView {
    pub generated_at_ms: u64,
    pub nodes: Vec<NodeStatus>,
    pub summary: FleetSummary,
}
#[derive(Parser, Debug)]
#[command(name = "nauti-fleet", about = "Fleet-wide nauti-daemon telemetry aggregator")]
struct Args {
    #[arg(long, default_value = "127.0.0.1", env = "NAUTI_FLEET_BIND")]
    bind: String,
    #[arg(long, default_value_t = 9122, env = "NAUTI_FLEET_PORT")]
    port: u16,
    /// Node list as `name=host[:port]`, comma separated.
    #[arg(
        long,
        env = "NAUTI_FLEET_NODES",
        default_value = "t440=127.0.0.1,cesarops2=192.168.4.24,cesarops3=192.168.4.31,cesarops4om=192.168.4.39"
    )]
    nodes: String,
    #[arg(long, default_value_t = 4, env = "NAUTI_FLEET_TIMEOUT_SECS")]
    timeout_secs: u64,
    #[arg(long, default_value_t = 5, env = "NAUTI_FLEET_REFRESH_SECS")]
    refresh_secs: u64,
    /// Permit polling node daemons on non-loopback addresses.
    #[arg(long, default_value_t = false, env = "NAUTI_FLEET_ALLOW_REMOTE")]
    allow_remote: bool,
    /// Directory holding the built WASM HUD (index.html + /nfui assets).
    #[arg(long, default_value = "/var/www/nauti-fleet", env = "NAUTI_FLEET_UI_DIR")]
    ui_dir: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    let mut targets: Vec<(String, String)> = Vec::new();
    for entry in args.nodes.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (name, hostport) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("bad node spec {entry}, want name=host[:port]"))?;
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(9121)),
            None => (hostport.to_string(), 9121),
        };
        targets.push((name.trim().to_string(), format!("http://{host}:{port}")));
    }
    if targets.is_empty() {
        anyhow::bail!("no nodes configured");
    }

    info!("nauti-fleet aggregating {} nodes", targets.len());
    let state = Arc::new(RwLock::new(FleetView {
        generated_at_ms: 0,
        nodes: Vec::new(),
        summary: FleetSummary::default(),
    }));

    tokio::spawn(refresh_loop(
        state.clone(),
        targets,
        Duration::from_secs(args.timeout_secs),
        Duration::from_secs(args.refresh_secs),
        args.allow_remote,
    ));

    let app = Router::new()
        .route("/health", get(aggregator_health))
        .route("/api/fleet/telemetry", get(fleet_telemetry))
        .route("/api/fleet/health", get(fleet_health))
        .route("/api/fleet/summary", get(fleet_summary))
        .route("/api/fleet/nodes", get(fleet_nodes))
        .route("/mcp", post(mcp_dispatch))
        .with_state(state);

    // Serve the built HUD from the same origin as the API, so the WASM app can
    // call /api/fleet/telemetry without CORS and without a second server.
    // Missing dir is not fatal — the API stays up and /api/health still answers.
    let ui_dir = args.ui_dir.clone();
    let app = if std::path::Path::new(&ui_dir).join("index.html").exists() {
        info!("serving HUD from {ui_dir}");
        app.fallback_service(
            ServeDir::new(&ui_dir).not_found_service(ServeFile::new(ui_dir.clone() + "/index.html")),
        )
    } else {
        warn!("no HUD at {ui_dir}/index.html — API-only mode");
        app
    };

    let addr = format!("{}:{}", args.bind, args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("nauti-fleet listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn refresh_loop(
    state: Arc<RwLock<FleetView>>,
    targets: Vec<(String, String)>,
    timeout: Duration,
    refresh: Duration,
    allow_remote: bool,
) {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .expect("client");

    loop {
        let mut nodes = Vec::with_capacity(targets.len());

        for (name, base) in &targets {
            // Each node's daemon binds localhost by design. A non-loopback
            // target only makes sense when the operator explicitly opted in;
            // refuse otherwise rather than silently polling across the LAN.
            let is_local = base.contains("127.0.0.1") || base.contains("localhost");
            if !is_local && !allow_remote {
                nodes.push(NodeStatus {
                    name: name.clone(),
                    url: base.clone(),
                    reachable: false,
                    error: Some("remote node polling disabled (start with --allow-remote)".into()),
                    age_secs: 0,
                    health: None,
                    telemetry: None,
                });
                continue;
            }

            match poll_node(&client, base).await {
                Ok((health, telemetry)) => {
                    info!("node {name} ok ({} accel)", telemetry.accelerators.len());
                    nodes.push(NodeStatus {
                        name: name.clone(),
                        url: base.clone(),
                        reachable: true,
                        error: None,
                        age_secs: 0,
                        health: Some(health),
                        telemetry: Some(telemetry),
                    });
                }
                Err(e) => {
                    warn!("node {name} unreachable: {e}");
                    nodes.push(NodeStatus {
                        name: name.clone(),
                        url: base.clone(),
                        reachable: false,
                        error: Some(e.to_string()),
                        age_secs: 0,
                        health: None,
                        telemetry: None,
                    });
                }
            }
        }

        let summary = FleetSummary::compute(&nodes);
        info!(
            "fleet: {}/{} reachable, {} gpus, vram {}/{} GB",
            summary.nodes_reachable,
            summary.nodes_total,
            summary.gpus_total,
            summary.vram_used_bytes / 1_000_000_000,
            summary.vram_total_bytes / 1_000_000_000
        );

        {
            let mut w = state.write().await;
            *w = FleetView {
                generated_at_ms: now_ms(),
                nodes,
                summary,
            };
        }

        tokio::time::sleep(refresh).await;
    }
}

async fn poll_node(
    client: &reqwest::Client,
    base: &str,
) -> anyhow::Result<(NodeHealth, UnifiedTelemetry)> {
    let health: NodeHealth = client
        .get(format!("{base}/health"))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("health parse: {e}"))?;

    let telemetry: UnifiedTelemetry = client
        .get(format!("{base}/api/telemetry"))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("telemetry parse: {e}"))?;

    Ok((health, telemetry))
}

async fn aggregator_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "service": "nauti-fleet", "ts": now_ms() }))
}

async fn fleet_telemetry(State(state): State<Arc<RwLock<FleetView>>>) -> Json<FleetView> {
    Json(state.read().await.clone())
}

async fn fleet_health(State(state): State<Arc<RwLock<FleetView>>>) -> Json<serde_json::Value> {
    let v = state.read().await;
    let nodes: Vec<serde_json::Value> = v
        .nodes
        .iter()
        .map(|n| {
            serde_json::json!({
                "name": n.name,
                "reachable": n.reachable,
                "error": n.error,
                "health": n.health,
            })
        })
        .collect();
    Json(serde_json::json!({
        "generated_at_ms": v.generated_at_ms,
        "nodes": nodes,
        "summary": v.summary,
    }))
}

async fn fleet_summary(State(state): State<Arc<RwLock<FleetView>>>) -> Json<FleetSummary> {
    Json(state.read().await.summary.clone())
}

async fn fleet_nodes(State(state): State<Arc<RwLock<FleetView>>>) -> Json<serde_json::Value> {
    let v = state.read().await;
    let nodes: Vec<serde_json::Value> = v
        .nodes
        .iter()
        .map(|n| {
            serde_json::json!({
                "name": n.name,
                "url": n.url,
                "reachable": n.reachable,
                "error": n.error,
                "age_secs": n.age_secs,
                "os": n.telemetry.as_ref().map(|t| t.os.clone()),
                "gpus": n.telemetry.as_ref().map(|t| t.accelerators.len()).unwrap_or(0),
            })
        })
        .collect();
    Json(serde_json::json!({ "nodes": nodes, "summary": v.summary }))
}

// ── MCP surface ─────────────────────────────────────────────────────────────

/// Minimal JSON-RPC 2.0 dispatch. Tools return the same payload as the HTTP
/// routes so an agent can use either without a second schema.
async fn mcp_dispatch(
    // axum requires the body-consuming extractor to come last; State is not
    // an extractor-from-parts and must precede it.
    State(state): State<Arc<RwLock<FleetView>>>,
    Json(req): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let params = req.get("params").cloned().unwrap_or(serde_json::Value::Null);

    let result: anyhow::Result<serde_json::Value> = async {
        match method.as_str() {
            "tools/list" => Ok(serde_json::json!({
                "tools": [
                    {"name": "fleet_telemetry", "description": "Full per-node telemetry snapshots."},
                    {"name": "fleet_health",    "description": "Per-node daemon health and reachability."},
                    {"name": "fleet_summary",   "description": "Compact fleet capacity/VRAM rollup."},
                    {"name": "fleet_vram_leaks","description": "Accelerators with VRAM pinned by a process."}
                ]
            })),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let v = state.read().await;
                let payload = match name.as_str() {
                    "fleet_telemetry" => serde_json::to_value(&*v)?,
                    "fleet_health" => serde_json::json!({
                        "generated_at_ms": v.generated_at_ms,
                        "nodes": v.nodes.iter().map(|n| serde_json::json!({
                            "name": n.name, "reachable": n.reachable,
                            "error": n.error, "health": n.health
                        })).collect::<Vec<_>>(),
                        "summary": v.summary,
                    }),
                    "fleet_summary" => serde_json::to_value(&v.summary)?,
                    "fleet_vram_leaks" => {
                        let mut leaks = Vec::new();
                        for n in &v.nodes {
                            if let Some(t) = &n.telemetry {
                                for g in &t.accelerators {
                                    if g.vram_used > 0 {
                                        leaks.push(serde_json::json!({
                                            "node": n.name,
                                            "gpu": g.name,
                                            "vendor": g.vendor,
                                            "vram_used": g.vram_used,
                                            "vram_total": g.vram_total,
                                            "temp_celsius": g.temp_celsius,
                                        }));
                                    }
                                }
                            }
                        }
                        serde_json::json!({ "count": leaks.len(), "accelerators": leaks })
                    }
                    other => anyhow::bail!("unknown tool {other}"),
                };
                Ok(serde_json::json!({
                    "content": [{"type": "text", "text": serde_json::to_string_pretty(&payload)?}]
                }))
            }
            other => anyhow::bail!("unsupported method {other}"),
        }
    }
    .await;

    match result {
        Ok(r) => Json(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": r})),
        Err(e) => {
            error!("mcp error: {e}");
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": e.to_string()}
            }))
        }
    }
}
