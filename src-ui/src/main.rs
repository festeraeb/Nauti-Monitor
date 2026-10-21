//! nauti-ui -- fleet dashboard, compiled to WASM.
//!
//! Reads the single telemetry source (nauti-fleet :9122, via the /dash/api/
//! nginx proxy) and renders every node's accelerators.
//!
//! The two themes are deliberately NOT the same DOM with different colours --
//! they share data and helpers only. Swapping themes swaps the whole component
//! tree, because restyling one layout read as "same dashboard, dark mode":
//!
//!   "buick"    -- 1973 Buick GS / '69 Trans Am. Analog needle gauges with
//!                chrome bezels and a printed redline, brushed aluminium,
//!                warm amber, woodgrain, horizontal instrument strip.
//!   "concept"  -- far-future ship's bridge. KITT-style sweeping scanner,
//!                segmented LED arcs (no needles at all), rotating rings,
//!                corner brackets, a vertical node register rail on the left
//!                and a wide HUD column on the right. Monospace throughout.
//!
//! Every gauge in both themes has an explicit red zone, and every one of them
//! announces how stale its own data is (T-SEC).
//!
//! Data source of truth: GET /api/fleet/telemetry on nauti-fleet.
//! Never shells out to nvidia-smi; see TELEMETRY.md.

use dioxus::prelude::*;
use gloo_net::http::Request;
use gloo_timers::future::TimeoutFuture;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
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
    pub temp_celsius: Option<f32>,
    pub power_watts: Option<f32>,
    pub model: Option<ModelServingInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NodeHealth {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub sysinfo_ok: bool,
    #[serde(default)]
    pub all_smi_ok: bool,
    #[serde(default)]
    pub polaris_fallback_active: bool,
    #[serde(default)]
    pub lemonade_reachable: bool,
    #[serde(default)]
    pub bmc_detected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ModelServingInfo {
    /// Name of the model currently loaded on this GPU (or primary model if multi-model)
    pub model_name: Option<String>,
    /// Engine/serve backend: "llama-server", "vllm", "tabbyapi", "poly-strata", "omarchy-agent", etc.
    pub engine: Option<String>,
    /// API endpoint where this model is served (e.g., "http://10.44.0.13:8106/v1")
    pub endpoint: Option<String>,
    /// VRAM allocated to this model (bytes)
    pub vram_allocated: Option<u64>,
    /// System RAM offloaded for this model (bytes) - for CPU offloading
    pub ram_offloaded: Option<u64>,
    /// Current tokens/sec (instantaneous)
    pub tps_current: Option<f32>,
    /// Average tokens/sec over recent window
    pub tps_avg: Option<f32>,
    /// Best/peak tokens/sec observed
    pub tps_best: Option<f32>,
    /// Context length configured
    pub context_length: Option<u32>,
    /// Quantization / precision
    pub quantization: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UnifiedTelemetry {
    #[serde(default)]
    pub node_name: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub cpu_pct: f32,
    #[serde(default)]
    pub memory_used_bytes: u64,
    #[serde(default)]
    pub memory_total_bytes: u64,
    #[serde(default)]
    pub rx_bps: f64,
    #[serde(default)]
    pub tx_bps: f64,
    #[serde(default)]
    pub accelerators: Vec<GpuStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NodeStatus {
    pub name: String,
    pub reachable: bool,
    pub error: Option<String>,
    pub health: Option<NodeHealth>,
    pub telemetry: Option<UnifiedTelemetry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct FleetSummary {
    pub nodes_total: usize,
    pub nodes_reachable: usize,
    pub gpus_total: usize,
    pub gpus_busy: usize,
    pub vram_total_bytes: u64,
    pub vram_used_bytes: u64,
    pub vram_free_bytes: u64,
    pub max_temp_c: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct FleetView {
    pub generated_at_ms: u64,
    pub nodes: Vec<NodeStatus>,
    pub summary: FleetSummary,
}

// ---------------------------------------------------------------- helpers ---

fn gb(b: u64) -> f32 {
    b as f32 / 1_073_741_824.0
}
fn pct(used: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { (used as f32 / total as f32) * 100.0 }
}
fn share(part: usize, whole: usize) -> f32 {
    if whole == 0 { 0.0 } else { part as f32 / whole as f32 }
}
fn clamp01(f: f32) -> f32 {
    f.clamp(0.0, 1.0)
}

/// Needle angle for a 240-degree sweep gauge, clamped to the dial.
fn needle_deg(frac: f32) -> f32 {
    -120.0 + clamp01(frac) * 240.0
}

/// Anything at or above this fraction of full scale is in the red zone.
/// One constant so both themes agree on what "redline" means.
const REDLINE: f32 = 0.833;

/// Temperature dial runs 0..100 C absolute, so the red zone lands on a real
/// junction temp (~83 C) rather than on "83% of whatever the box reports".
const TEMP_FULL: f32 = 100.0;
/// GPU junction temp above which a row is hot.
const TEMP_HOT: f32 = 82.0;

fn is_red(frac: f32) -> bool {
    frac >= REDLINE
}

/// Human bandwidth. The API's `rx_bps`/`tx_bps` are bytes/sec despite the
/// name, which is what nauti-fleet emits -- label them B/s, not bit/s.
fn human_bps(v: f64) -> String {
    if v >= 1e9 {
        format!("{:.2} GB/s", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.1} MB/s", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.0} kB/s", v / 1e3)
    } else {
        format!("{:.0} B/s", v)
    }
}

/// Wall clock in ms. chrono's `wasmbind` feature routes this to Date::now()
/// on wasm32-unknown-unknown, so no js_sys dependency is needed.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// H:MM:SS from epoch ms, UTC (same basis the payload was stamped on).
fn hhmmss(ms: u64) -> String {
    if ms == 0 {
        return "--:--:--".to_string();
    }
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60, s % 60)
}

#[derive(Clone, Copy, PartialEq)]
enum Fresh {
    Boot,
    Live,
    Stale,
    Lost,
}

/// Age of the freshest data we are still rendering.
///
/// nauti-fleet answers about once a second. Under 2.5s is live. 2.5-8s means
/// we are holding a cached frame and must SAY so -- otherwise a dead backend
/// leaves a plausible-looking dashboard frozen mid-scale, which is the single
/// most dangerous failure mode for a status page. Past 8s the link is gone.
fn freshness(age: f64) -> Fresh {
    if age < 0.0 {
        Fresh::Boot
    } else if age < 2.5 {
        Fresh::Live
    } else if age < 8.0 {
        Fresh::Stale
    } else {
        Fresh::Lost
    }
}

fn fresh_word(f: Fresh) -> &'static str {
    match f {
        Fresh::Boot => "BOOT",
        Fresh::Live => "LIVE",
        Fresh::Stale => "STALE",
        Fresh::Lost => "LOST",
    }
}

/// One poll of telemetry. Shared by both themes -- only the presentation
/// diverges, never the source.
async fn poll() -> Option<FleetView> {
    // Path must match the nginx proxy in snippets/cesarops-nauti-monitor.conf.
    // It is /dash/-namespaced because the bare /api/fleet/ prefix already
    // belongs to wrecks-api (:8099, SSH bootstrap). Same-origin so the portal
    // cookie rides along and the auth_request gate passes.
    // NOTE: testing straight against nauti-fleet on :9122 (which serves the
    // bundle at /) this 404s -- go through nginx instead.
    let res = Request::get("/dash/api/fleet/telemetry").send().await.ok()?;
    if res.status() != 200 {
        return None;
    }
    res.json::<FleetView>().await.ok()
}

fn main() {
    // Panic -> console.error. Blank WASM with no message is undebuggable.
    console_error_panic_hook::set_once();
    dioxus::launch(App);
}

#[component]
pub fn App() -> Element {
    let mut view = use_signal(|| None::<FleetView>);
    let mut theme = use_signal(|| "buick".to_string());
    // Rolling history of fleet VRAM utilisation, for the trend strip.
    let mut history = use_signal(|| Vec::<f32>::new());
    // Wall-clock ms of the last SUCCESSFUL decode; 0 = never succeeded.
    let mut rx_at = use_signal(|| 0_i64);
    let mut why = use_signal(|| String::new());
    // Independent 4 Hz wall clock. Without this T-SEC only advances when data
    // arrives, so a dead backend would freeze the counter at its last value
    // and look healthy.
    let mut clock = use_signal(|| now_ms());
    // Name of the node opened in the detail overlay.
    let mut sel = use_signal(|| None::<String>);

    use_future(move || async move {
        loop {
            match poll().await {
                Some(v) => {
                    let mut h = history.read().clone();
                    h.push(pct(v.summary.vram_used_bytes, v.summary.vram_total_bytes));
                    if h.len() > 60 {
                        h.remove(0);
                    }
                    history.set(h);
                    view.set(Some(v));
                    rx_at.set(now_ms());
                    why.set(String::new());
                }
                None => {
                    why.set("request failed or payload unparseable".to_string());
                }
            }
            TimeoutFuture::new(1000).await;
        }
    });

    use_future(move || async move {
        loop {
            TimeoutFuture::new(250).await;
            clock.set(now_ms());
        }
    });

    let age = if rx_at() == 0 {
        -1.0
    } else {
        (clock() - rx_at()) as f64 / 1000.0
    };
    let fresh = freshness(age);
    let tsec = if age < 0.0 {
        "--.-".to_string()
    } else {
        format!("{:.1}", age.min(999.0))
    };

    let on_pick = move |name: String| sel.set(Some(name));

    rsx! {
        style { {include_str!("./styles.css")} }
        main {
            class: "app",
            "data-theme": "{theme}",
            "data-fresh": "{fresh_word(fresh)}",
            Header {
                theme: theme,
                on_toggle: move |t| theme.set(t),
                fresh: fresh,
                tsec: tsec.clone(),
                summary: view().map(|v| v.summary.clone()),
            }
            // The two themes share nothing but `view`. Gated on Some() so the
            // first paint shows a boot message instead of an all-zero deck
            // that would read as "fleet is idle" rather than "no data yet".
            if let Some(v) = view() {
                if theme() == "concept" {
                    ConceptHud {
                        view: v.clone(),
                        history: history().clone(),
                        fresh,
                        tsec: tsec.clone(),
                        on_pick: on_pick.clone(),
                    }
                } else if theme() == "drift" {
                    DriftDeck {
                        view: v.clone(),
                        history: history().clone(),
                        fresh,
                        tsec: tsec.clone(),
                        on_pick: on_pick.clone(),
                    }
                } else {
                    BuickDeck {
                        view: v.clone(),
                        history: history().clone(),
                        fresh,
                        tsec: tsec.clone(),
                        on_pick: on_pick.clone(),
                    }
                }
            } else {
                div { class: "boot", "Acquiring fleet telemetry stream…" }
            }
            Footer {
                fresh,
                tsec: tsec.clone(),
                why: why(),
                stamp: view().map(|v| v.generated_at_ms).unwrap_or(0),
            }
            if let Some(name) = sel() {
                if let Some(node) = view()
                    .as_ref()
                    .and_then(|v| v.nodes.iter().find(|n| n.name == name).cloned())
                {
                    Detail {
                        node,
                        fresh,
                        tsec: tsec.clone(),
                        on_close: move |_| sel.set(None),
                    }
                }
            }
        }
    }
}

// ----------------------------------------------------------------- header ---

#[component]
fn Header(
    theme: Signal<String>,
    on_toggle: EventHandler<String>,
    fresh: Fresh,
    tsec: String,
    summary: Option<FleetSummary>,
) -> Element {
    let s = summary.unwrap_or_default();
    let fw = fresh_word(fresh);
    rsx! {
        header { class: "hdr",
            div { class: "hdr-brand",
                div { class: "mark" }
                div {
                    h1 { "NAUTI FLEET" }
                    p { class: "sub", "telemetry // single source of truth" }
                }
            }
            // T-SEC: seconds since the last good frame. Prominent on purpose --
            // it is the only thing that tells you whether the numbers are true.
            div { class: "hdr-tsec",
                span { class: "tsec-k", "T-SEC" }
                span { class: "tsec-v", "{tsec}" }
                span { class: "tsec-w", "{fw}" }
            }
            div { class: "hdr-stats",
                div { class: "hs", span { class: "hs-v", "{s.nodes_reachable}/{s.nodes_total}" } span { class: "hs-k", "NODES" } }
                div { class: "hs", span { class: "hs-v", "{s.gpus_busy}/{s.gpus_total}" } span { class: "hs-k", "GPU BUSY" } }
                div { class: "hs", span { class: "hs-v", "{gb(s.vram_used_bytes):.0}/{gb(s.vram_total_bytes):.0}G" } span { class: "hs-k", "VRAM" } }
                div { class: "hs", span { class: "hs-v", "{s.max_temp_c.unwrap_or(0.0):.0}C" } span { class: "hs-k", "PEAK" } }
            }
            div { class: "hdr-r",
                button {
                    class: if theme() == "buick" { "tg sel" } else { "tg" },
                    onclick: move |_| on_toggle.call("buick".to_string()),
                    "BUICK '73"
                }
                button {
                    class: if theme() == "concept" { "tg sel" } else { "tg" },
                    onclick: move |_| on_toggle.call("concept".to_string()),
                    "CONCEPT"
                }
                button {
                    class: if theme() == "drift" { "tg sel" } else { "tg" },
                    onclick: move |_| on_toggle.call("drift".to_string()),
                    "DRIFT"
                }
            }
        }
    }
}

// ------------------------------------------------------------ polar maths ---

/// Point on the dial at fraction `frac` of the 240-degree sweep. Angle is
/// measured clockwise from 12 o'clock, which is what SVG rotate() expects.
fn polar(r: f32, frac: f32) -> (f32, f32) {
    let a = needle_deg(frac).to_radians();
    (60.0 + r * a.sin(), 60.0 - r * a.cos())
}

/// SVG path for the arc spanning [f0, f1] at radius r. Derived from REDLINE
/// rather than hardcoded, so moving the redline moves the printed arc too.
fn arc(r: f32, f0: f32, f1: f32) -> String {
    let (x1, y1) = polar(r, f0);
    let (x2, y2) = polar(r, f1);
    let large = if (f1 - f0) * 240.0 > 180.0 { 1 } else { 0 };
    format!("M {x1:.2} {y1:.2} A {r} {r} 0 {large} 1 {x2:.2} {y2:.2}")
}

// ============================== THEME 1: BUICK '73 ==========================
// Horizontal instrument strip on a woodgrain shelf, analog needles, chrome
// bezels, printed redline, a paper-stock trend strip and a card grid below.

#[component]
fn BuickDeck(
    view: FleetView,
    history: Vec<f32>,
    fresh: Fresh,
    tsec: String,
    on_pick: EventHandler<String>,
) -> Element {
    let s = view.summary.clone();
    let util = pct(s.vram_used_bytes, s.vram_total_bytes) / 100.0;
    let reach = share(s.nodes_reachable, s.nodes_total);
    // Inverted: a full dial means every node is answering. Anything less than
    // full is the bad case, so the needle dropping into red is what you watch.
    let down = 1.0 - reach;
    let busy = share(s.gpus_busy, s.gpus_total);
    let tmax = s.max_temp_c.unwrap_or(0.0);
    let tfrac = (tmax / TEMP_FULL).clamp(0.0, 1.0);
    // Precomputed: format!/rsx! interpolations take identifiers and field
    // accesses only, never arithmetic expressions.
    let util_pct = util * 100.0;

    rsx! {
        div { class: "bk",
            section { class: "bk-strip",
                Gauge {
                    label: "VRAM LOAD".to_string(), frac: util,
                    // No "%" here: Gauge appends `unit` after `display`, so
                    // baking the symbol into both renders "53%%".
                    display: format!("{util_pct:.0}"),
                    sub: format!("{:.0} / {:.0} GB", gb(s.vram_used_bytes), gb(s.vram_total_bytes)),
                    unit: "%".to_string(), red_at: 100.0
                }
                Gauge {
                    label: "NODES DOWN".to_string(), frac: down,
                    display: format!("{}", s.nodes_total - s.nodes_reachable),
                    sub: format!("of {} total", s.nodes_total),
                    unit: "".to_string(), red_at: 0.0
                }
                Gauge {
                    label: "GPUS BUSY".to_string(), frac: busy,
                    display: format!("{}", s.gpus_busy),
                    sub: format!("of {} total", s.gpus_total),
                    unit: "".to_string(), red_at: 100.0
                }
                Gauge {
                    label: "PEAK TEMP".to_string(), frac: tfrac,
                    display: format!("{tmax:.0}"),
                    sub: "degrees C".to_string(),
                    unit: "C".to_string(), red_at: TEMP_FULL * REDLINE
                }
            }
            section { class: "bk-beside",
                div { class: "bk-trend",
                    div { class: "bk-trend-h", "VRAM TREND // LAST 60s" }
                    Trend { history: history.clone(), thin: false }
                }
                div { class: if is_red(util) { "bk-flag red" } else { "bk-flag" },
                    span { class: "bk-lamp" }
                    div {
                        div { class: "bk-flag-t",
                            if is_red(util) { "HIGH LOAD -- VRAM IN THE REDLINE" }
                            else { "ALL GAUGES IN NORMAL BAND" }
                        }
                        div { class: "bk-flag-s", "last frame {tsec}s ago" }
                    }
                }
            }
            section { class: "bk-grid",
                for n in view.nodes.iter() {
                    NodeCard { node: n.clone(), on_pick: on_pick.clone() }
                }
            }
        }
    }
}

/// ----------------------------------------------------------------- drift ---
///
/// "Shipwreck & sailboat" theme. Weathered wood, ocean depths, sailcloth,
/// rope and driftwood. Every node is a hull; every model cluster is a sail,
/// pickled in its own racing-stripe colour. Per-gun planks show token
/// generation/sec and blink amber when throttled. The fleet readout is a
/// ship's log with a compass bearing for staleness (T-SEC).
#[component]
fn DriftDeck(
    view: FleetView,
    history: Vec<f32>,
    fresh: Fresh,
    tsec: String,
    on_pick: EventHandler<String>,
) -> Element {
    let s = view.summary.clone();
    let n_gpu = s.gpus_total;
    let n_busy = s.gpus_busy;
    let tmax = s.max_temp_c.unwrap_or(0.0);
    let fw = fresh_word(fresh);

    rsx! {
        div { class: "drift-deck",
            div { class: "drift-log",
                div { class: "drift-log-h",
                    h2 { "SHIP'S LOG // NAUTI FLEET" }
                    span { class: "drift-tsec", "{fw}  T-SEC {tsec}" }
                    div { class: "drift-log-stats",
                        span { "{s.nodes_reachable}/{s.nodes_total} hulls" }
                        span { "{n_busy}/{n_gpu} sails aloft" }
                        span { "{gb(s.vram_used_bytes):.0}/{gb(s.vram_total_bytes):.0}G" }
                        span { "{tmax:.0}C peak" }
                    }
                }
            }
            div { class: "drift-fleet",
                for n in view.nodes.iter() {
                    DriftNode { node: n.clone(), on_pick: on_pick.clone() }
                }
                if view.nodes.is_empty() {
                    div { class: "drift-none", "No hulls in the log — awaiting first frame." }
                }
            }
        }
    }
}

/// One hull (node) in the drift deck: weathered wood card with its model
/// clusters as sails, each sail carrying its own racing-stripe colour.
#[component]
fn DriftNode(node: NodeStatus, on_pick: EventHandler<String>) -> Element {
    let name = node.name.clone();
    let tel = node.telemetry.clone().unwrap_or_default();
    let accels = tel.accelerators.clone();
    let n_gpu = accels.len();
    let up = node.reachable;
    let cpu = tel.cpu_pct;
    let ram = pct(tel.memory_used_bytes, tel.memory_total_bytes);
    let rx = human_bps(tel.rx_bps);
    let tx = human_bps(tel.tx_bps);
    let clusters = group_clusters(&accels);
    let card_cls = if !up {
        "drift-node down"
    } else if cpu >= REDLINE * 100.0 || ram >= REDLINE * 100.0 {
        "drift-node hot"
    } else {
        "drift-node"
    };
    let up_txt = if up { "LINK" } else { "DOWN" };
    let err_txt = node.error.clone().unwrap_or_else(|| "none".to_string());

    rsx! {
        article {
            class: card_cls,
            onclick: move |_| on_pick.call(name.clone()),
            div { class: "drift-hd",
                div { class: "drift-hull",
                    span { class: "drift-dot" }
                    h2 { "{name}" }
                }
                span { class: "drift-state", "{up_txt}" }
            }
            div { class: "drift-os", "{tel.os}" }
            div { class: "drift-mini",
                MiniBar { label: "CPU".to_string(), v: cpu }
                MiniBar { label: "RAM".to_string(), v: ram }
            }
            if up {
                div { class: "drift-net",
                    span { class: "drift-k", "RX" }
                    span { class: "drift-v", "{rx}" }
                    span { class: "drift-k", "TX" }
                    span { class: "drift-v", "{tx}" }
                }
                div { class: "drift-sails",
                    for cluster in clusters.iter() {
                        div { class: if cluster.model == "— idle —" { "drift-sail idle" } else { "drift-sail" },
                            div { class: "drift-stripe", style: "background-color: {cluster.stripe_color}" }
                            if cluster.model != "— idle —" {
                                div { class: "drift-sail-hd",
                                    span { class: "drift-sail-name", "{cluster.model}" }
                                    span { class: "drift-sail-tps",
                                        "now {cluster.tps_now:.1}  avg {cluster.tps_avg:.1}  peak {cluster.tps_peak:.1} t/s"
                                    }
                                    if cluster.ram_gb > 0.0 {
                                        span { class: "drift-sail-ram", "↕ {cluster.ram_gb:.1} GB offload" }
                                    }
                                    span { class: "drift-sail-count", "{cluster.gpus.len()} guns" }
                                }
                            }
                            div { class: "drift-planks",
                                for (i, g) in cluster.gpus.iter() {
                                    DriftPlank { idx: *i, g: g.clone() }
                                }
                            }
                        }
                    }
                }
                if n_gpu == 0 {
                    div { class: "drift-none", "no accelerators reported" }
                }
            } else {
                div { class: "drift-down",
                    span { class: "drift-down-t", "HULL LOST" }
                    code { "{err_txt}" }
                }
            }
        }
    }
}

/// One plank (GPU) in a drift sail: per-GPU stats with token generation/sec
/// and an amber THROTTLED blink when generation is stalled.
#[component]
fn DriftPlank(idx: usize, g: GpuStats) -> Element {
    let v = pct(g.vram_used, g.vram_total);
    let t = g.temp_celsius.unwrap_or(0.0);
    let hot = t >= TEMP_HOT || v >= REDLINE * 100.0;
    let nm = g.name.clone();
    let watts = g.power_watts.map(|p| format!("{p:.0}W")).unwrap_or_else(|| "--".into());
    let util = g.utilization;
    let used_gb = gb(g.vram_used);
    let total_gb = gb(g.vram_total);
    let throttled = g.model.as_ref().map_or(false, |m| {
        let best = m.tps_best.unwrap_or(0.0);
        let cur = m.tps_current.unwrap_or(0.0);
        best > 0.0 && (cur == 0.0 || cur < best * 0.30)
    });
    // Hoisted out of rsx!: a nested string literal inside a text
    // interpolation can't be tokenised by the Rust lexer.
    let model_name = g
        .model
        .as_ref()
        .and_then(|m| m.model_name.clone())
        .unwrap_or_else(|| "—".to_string());

    rsx! {
        div { class: if hot { "drift-plank hot" } else { "drift-plank" },
            div { class: "drift-plank-hd",
                span { class: "drift-plank-ix", "GUN {idx}" }
                span { class: "drift-plank-nm", "{nm}" }
                span { class: "drift-plank-u", "{util:.0}%" }
                if throttled { span { class: "drift-throttle blink", "THROTTLED" } }
            }
            if let Some(ref m) = g.model {
                div { class: "drift-plank-model",
                    span { class: "drift-model-name", "{model_name}" }
                    span { class: "drift-model-engine", "{m.engine.clone().unwrap_or_default()}" }
                    if let Some(tps) = m.tps_current {
                        span { class: "drift-model-tps", "{tps:.1} t/s" }
                    }
                }
            }
            div { class: "drift-plank-f",
                span { class: if t >= TEMP_HOT { "hot" } else { "" }, "{t:.1} C" }
                span { "{watts}" }
                span { "{used_gb:.1}/{total_gb:.1} GB" }
            }
            Bar { v, kind: "vram".to_string() }
            Bar { v: (t / TEMP_FULL) * 100.0, kind: "temp".to_string() }
        }
    }
}

/// Analog dial: chrome bezel, printed redline arc, swinging amber needle.
///
/// `frac` MUST be 0.0..=1.0 of full scale -- the angle comes from it directly.
/// Passing a 0..100 percentage pins the needle at full scale; normalise at the
/// call site. `red_at` is only the ceiling label (100 for a %, 100 for C).
#[component]
fn Gauge(
    label: String,
    frac: f32,
    display: String,
    sub: String,
    unit: String,
    red_at: f32,
) -> Element {
    let deg = needle_deg(frac);
    let red = is_red(frac);
    let cls = if red { "gauge red" } else { "gauge" };
    let deg_s = format!("{deg:.1}");
    let red_ceiling = if unit.is_empty() { String::new() } else { format!("{}", red_at as i32) };

    // Precompute tick geometry outside rsx! -- nesting a second rsx! block
    // inside a `for` inside rsx! does not parse in dioxus 0.6.
    let ticks: Vec<(String, String, String, String, String)> = (0..=12)
        .map(|i| {
            let f = i as f32 / 12.0;
            let (x1, y1) = polar(40.0, f);
            let (x2, y2) = polar(46.0, f);
            (
                format!("{x1:.1}"),
                format!("{y1:.1}"),
                format!("{x2:.1}"),
                format!("{y2:.1}"),
                if f >= REDLINE { "g-tick in-red".to_string() } else { "g-tick".to_string() },
            )
        })
        .collect();
    // Minor ticks inside the redline, so the red band reads as a scale and not
    // just a splash of paint.
    let red_ticks: Vec<(String, String, String, String)> = (0..5)
        .map(|i| {
            let f = REDLINE + (i as f32 / 4.0) * (1.0 - REDLINE);
            let (x1, y1) = polar(34.0, f);
            let (x2, y2) = polar(40.0, f);
            (format!("{x1:.1}"), format!("{y1:.1}"), format!("{x2:.1}"), format!("{y2:.1}"))
        })
        .collect();

    rsx! {
        div { class: cls,
            div { class: "g-face",
                svg { view_box: "0 0 120 120", class: "g-svg",
                    circle { cx: "60", cy: "60", r: "56", class: "g-rim" }
                    circle { cx: "60", cy: "60", r: "49", class: "g-inset" }
                    // Printed red band behind the ticks.
                    path { d: arc(40.0, REDLINE, 1.0), class: "g-band" }
                    path { d: arc(46.0, 0.0, REDLINE), class: "g-ok" }
                    for (x1, y1, x2, y2, tc) in ticks.iter() {
                        line { x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}", class: "{tc}" }
                    }
                    for (x1, y1, x2, y2) in red_ticks.iter() {
                        line { x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}", class: "g-rtick" }
                    }
                    // Single-arg rotate: the pivot lives in CSS (transform-origin
                    // + transform-box: view-box). Baking the centre into the
                    // attribute too makes Chrome apply it twice, which throws
                    // the needle clear outside the dial.
                    g { class: "g-needle", transform: "rotate({deg_s})",
                        polygon { points: "60,15 63.2,60 60,66 56.8,60" }
                    }
                    circle { cx: "60", cy: "60", r: "6.5", class: "g-hub" }
                    circle { cx: "60", cy: "60", r: "2.4", class: "g-hubpin" }
                }
                div { class: "g-read",
                    span { class: "g-num", "{display}" }
                    if !unit.is_empty() { span { class: "g-unit", "{unit}" } }
                }
                div { class: "g-lamp" }
            }
            div { class: "g-lbl", "{label}" }
            div { class: "g-sub",
                "{sub}"
                if !red_ceiling.is_empty() { " // red at {red_ceiling}{unit}" }
            }
        }
    }
}

/// 60-sample trend strip. Shared geometry, restyled per theme.
#[component]
fn Trend(history: Vec<f32>, thin: bool) -> Element {
    let line = spark_points(&history);
    let fill = spark_fill(&history);
    rsx! {
        div { class: if thin { "trend thin" } else { "trend" },
            svg { view_box: "0 0 240 46", preserve_aspect_ratio: "none",
                // Redline ceiling at the same fraction as every other gauge.
                line { x1: "0", y1: "{44.0 - REDLINE * 40.0}", x2: "240",
                    y2: "{44.0 - REDLINE * 40.0}", class: "trend-red" }
                polyline { class: "trend-fill", points: "{fill}" }
                polyline { class: "trend-line", points: "{line}" }
            }
        }
    }
}

/// Build an SVG polyline "x,y x,y" across a 240x46 box.
fn spark_points(h: &[f32]) -> String {
    if h.len() < 2 {
        return String::new();
    }
    let step = 240.0 / (h.len().saturating_sub(1)) as f32;
    h.iter()
        .enumerate()
        .map(|(i, v)| {
            let x = i as f32 * step;
            let y = 44.0 - clamp01(v / 100.0) * 40.0;
            format!("{x:.1},{y:.1}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn spark_fill(h: &[f32]) -> String {
    if h.len() < 2 {
        return String::new();
    }
    let line = spark_points(h);
    format!("0,46 {line} 240.0,46")
}

// ============================== THEME 2: CONCEPT =============================
// No needles anywhere. KITT scanner, segmented LED arcs, rotating rings,
// corner brackets, a node register rail and a wide HUD well. Monospace.

#[component]
fn ConceptHud(
    view: FleetView,
    history: Vec<f32>,
    fresh: Fresh,
    tsec: String,
    on_pick: EventHandler<String>,
) -> Element {
    let s = view.summary.clone();
    let util = pct(s.vram_used_bytes, s.vram_total_bytes) / 100.0;
    let reach = share(s.nodes_reachable, s.nodes_total);
    let busy = share(s.gpus_busy, s.gpus_total);
    let tmax = s.max_temp_c.unwrap_or(0.0);
    let tfrac = (tmax / TEMP_FULL).clamp(0.0, 1.0);
    let util_pct = util * 100.0;

    // Anything currently in a red zone, named -- the rail is only useful if it
    // says *which* item is bad rather than lighting a lamp somewhere.
    let alerts: Vec<String> = view
        .nodes
        .iter()
        .flat_map(|n| {
            let mut v = Vec::new();
            if !n.reachable {
                v.push(format!("{} :: node unreachable", n.name));
            }
            let tel = n.telemetry.clone().unwrap_or_default();
            if tel.cpu_pct >= REDLINE * 100.0 {
                v.push(format!("{} :: CPU {:.0}%", n.name, tel.cpu_pct));
            }
            for (i, g) in tel.accelerators.iter().enumerate() {
                if pct(g.vram_used, g.vram_total) >= REDLINE * 100.0 {
                    v.push(format!("{} :: gpu{} VRAM {:.0}%", n.name, i, pct(g.vram_used, g.vram_total)));
                }
                if let Some(t) = g.temp_celsius {
                    if t >= TEMP_HOT {
                        v.push(format!("{} :: gpu{} TEMP {:.0}C", n.name, i, t));
                    }
                }
            }
            v
        })
        .collect();

    rsx! {
        div { class: "cp",
            // --- KITT scanner ------------------------------------------------- 
            div { class: "cp-scan",
                KittScanner { load: util, red: is_red(util) || fresh == Fresh::Lost }
                div { class: "cp-scan-cap",
                    span { "SCANNER // FLOOR LOAD" }
                    span { class: "cp-scan-v", "{util_pct:.0}%" }
                }
            }
            div { class: "cp-body",
                // --- left: node register -------------------------------------- 
                aside { class: "cp-rail",
                    div { class: "cp-rail-h", "NODE REGISTER" }
                    div { class: "cp-rail-sub", "click a node for full detail" }
                    for n in view.nodes.iter() {
                        RegRow { node: n.clone(), on_pick: on_pick.clone() }
                    }
                }
                // --- centre: segmented arcs ----------------------------------- 
                section { class: "cp-well",
                    div { class: "cp-well-h",
                        span { "FLEET PROPULSION" }
                        span { class: "cp-well-t", "T+{tsec}s" }
                    }
                    div { class: "cp-arcs",
                        SegDial {
                            label: "VRAM".to_string(), frac: util,
                            display: format!("{:.0}", util * 100.0), unit: "%".to_string(),
                            sub: format!("{:.0}G / {:.0}G", gb(s.vram_used_bytes), gb(s.vram_total_bytes)),
                        }
                        SegDial {
                            label: "LINK".to_string(), frac: reach,
                            display: format!("{}", s.nodes_reachable), unit: "".to_string(),
                            sub: format!("of {} nodes", s.nodes_total),
                        }
                        SegDial {
                            label: "THROTTLE".to_string(), frac: busy,
                            display: format!("{}", s.gpus_busy), unit: "".to_string(),
                            sub: format!("of {} gpus", s.gpus_total),
                        }
                        SegDial {
                            label: "THERMAL".to_string(), frac: tfrac,
                            display: format!("{:.0}", tmax), unit: "C".to_string(),
                            sub: "peak junction".to_string(),
                        }
                    }
                    div { class: "cp-trend",
                        div { class: "cp-trend-h", "LOAD HISTORY 60s" }
                        Trend { history, thin: true }
                    }
                }
                // --- right: alert rail ---------------------------------------- 
                aside { class: "cp-alerts",
                    div { class: "cp-alerts-h", "RED ZONE" }
                    if alerts.is_empty() {
                        div { class: "cp-clear", "ALL BANDS CLEAR" }
                    }
                    for a in alerts.iter() {
                        div { class: "cp-alert", "{a}" }
                    }
                }
            }
        }
    }
}

/// Number of LED segments in a conceptual arc, and in a scanner strip.
const SEGS: usize = 24;
const SCAN_SEGS: usize = 30;

/// A segmented arc instead of a needle: 24 LED blocks around a 240-degree
/// sweep, the last four of which are the red zone. When the value is IN the
/// red zone the whole arc pulses, so it is obvious from across the room.
#[component]
fn SegDial(label: String, frac: f32, display: String, unit: String, sub: String) -> Element {
    let f = clamp01(frac);
    let red = is_red(f);
    let lit_n = (f * SEGS as f32).ceil() as usize;
    let cls = if red { "segd in-red" } else { "segd" };
    let degs: Vec<String> = (0..SEGS)
        .map(|i| format!("{:.2}", -120.0 + (i as f32 + 0.5) * (240.0 / SEGS as f32)))
        .collect();
    let segcls: Vec<String> = (0..SEGS)
        .map(|i| {
            let inred = (i as f32 / SEGS as f32) >= REDLINE;
            let lit = i < lit_n;
            match (inred, lit) {
                (true, true) => "seg on red",
                (true, false) => "seg red",
                (false, true) => "seg on",
                (false, false) => "seg",
            }
            .to_string()
        })
        .collect();

    rsx! {
        div { class: cls,
            div { class: "segd-brackets",
                span { class: "bk-c tl" }
                span { class: "bk-c tr" }
                span { class: "bk-c bl" }
                span { class: "bk-c br" }
            }
            div { class: "segd-face",
                svg { view_box: "0 0 120 120", class: "segd-svg",
                    // Two counter-rotating reticles, purely decorative motion.
                    circle { cx: "60", cy: "60", r: "54", class: "ring spin-cw" }
                    circle { cx: "60", cy: "60", r: "50", class: "ring spin-ccw" }
                    for (i, d) in degs.iter().enumerate() {
                        rect {
                            x: "58.1",
                            y: "9",
                            width: "3.8",
                            height: "11",
                            rx: "1.2",
                            class: "{segcls[i]}",
                            transform: "rotate({d})",
                        }
                    }
                    circle { cx: "60", cy: "60", r: "31", class: "segd-well" }
                }
                div { class: "segd-read",
                    span { class: "segd-num", "{display}" }
                    if !unit.is_empty() { span { class: "segd-unit", "{unit}" } }
                }
            }
            div { class: "segd-lbl", "{label}" }
            div { class: "segd-sub", "{sub}" }
            div { class: "segd-zone",
                span { class: "z-ok", "OK" }
                if red {
                    span { class: "z-red blink", "REDLINE" }
                } else {
                    span { class: "z-red dim", "REDLINE" }
                }
            }
        }
    }
}

/// KITT-style sweep. The travelling highlight is a pure CSS animation; the
/// segment fill behind it carries the real number so the strip is a gauge and
/// not just a light show.
#[component]
fn KittScanner(load: f32, red: bool) -> Element {
    let f = clamp01(load);
    let lit_n = (f * SCAN_SEGS as f32).ceil() as usize;
    let cls = if red { "kitt in-red" } else { "kitt" };
    let cells: Vec<String> = (0..SCAN_SEGS)
        .map(|i| {
            let inred = (i as f32 / SCAN_SEGS as f32) >= REDLINE;
            let lit = i < lit_n;
            match (inred, lit) {
                (true, true) => "kc on red",
                (true, false) => "kc red",
                (false, true) => "kc on",
                (false, false) => "kc",
            }
            .to_string()
        })
        .collect();
    rsx! {
        div { class: cls,
            div { class: "kitt-cells",
                for c in cells.iter() {
                    span { class: "{c}" }
                }
            }
            div { class: "kitt-sweep" }
            div { class: "kitt-redzone" }
        }
    }
}

/// One row of the concept theme's node register. Compact by design -- the full
/// story is one click away in the detail overlay.
#[component]
fn RegRow(node: NodeStatus, on_pick: EventHandler<String>) -> Element {
    let name = node.name.clone();
    let pick = name.clone();
    let tel = node.telemetry.clone().unwrap_or_default();
    let up = node.reachable;
    let cpu = tel.cpu_pct;
    let vram = tel
        .accelerators
        .iter()
        .map(|g| pct(g.vram_used, g.vram_total))
        .fold(0.0f32, f32::max);
    let temp = tel.accelerators.iter().filter_map(|g| g.temp_celsius).fold(0.0f32, f32::max);
    let hot = temp >= TEMP_HOT || vram >= REDLINE * 100.0 || cpu >= REDLINE * 100.0;
    let cls = match (up, hot) {
        (false, _) => "reg down",
        (true, true) => "reg hot",
        (true, false) => "reg",
    };
    let segs: Vec<String> = (0..10)
        .map(|i| {
            let inred = (i as f32 / 10.0) >= REDLINE;
            let lit = (i as f32 / 10.0) < vram / 100.0;
            match (inred, lit) {
                (true, true) => "rs on red",
                (true, false) => "rs red",
                (false, true) => "rs on",
                (false, false) => "rs",
            }
            .to_string()
        })
        .collect();
    rsx! {
        button { class: cls, onclick: move |_| on_pick.call(pick.clone()),
            span { class: "reg-led" }
            span { class: "reg-nm", "{name}" }
            span { class: "reg-segs",
                for s in segs.iter() {
                    span { class: "{s}" }
                }
            }
            span { class: "reg-t", "{temp:.0}C" }
        }
    }
}

// ------------------------------------------------------------ shared pieces --

/// Pick a stable racing-stripe colour for a model/task. The same model always
/// maps to the same colour, so the eye can track a task's GPUs across the
/// whole fleet at a glance. Idle GPUs get a neutral dim stripe.
fn model_stripe_color(name: &str) -> String {
    const PALETTE: [&str; 10] = [
        "#e94560", // crimson
        "#ff6b35", // orange
        "#ffa502", // amber
        "#ffe66d", // solar yellow
        "#a8e6cf", // mint
        "#7ed6df", // cyan
        "#4facfe", // blue
        "#a18cd1", // slate purple
        "#ff9ff3", // orchid
        "#feca57", // gold
    ];
    if name == "— idle —" {
        return "#6c757d".to_string();
    }
    let mut h: u32 = 2166136261; // FNV offset basis
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619); // FNV prime
    }
    PALETTE[(h as usize) % PALETTE.len()].to_string()
}

#[derive(Clone)]
struct Cluster {
    model: String,
    gpus: Vec<(usize, GpuStats)>,
    tps_now: f32,
    tps_avg: f32,
    tps_peak: f32,
    ram_gb: f32,
    /// Racing-stripe colour for this model/task (hex #RRGGBB).
    stripe_color: String,
}

/// Group GPUs by the model they're serving. GPUs without a model go to a
/// trailing "— idle —" cluster. Order is stable (first-seen).
fn group_clusters(accels: &[GpuStats]) -> Vec<Cluster> {
    let mut by_model: std::collections::HashMap<String, Vec<(usize, GpuStats)>> =
        std::collections::HashMap::new();
    let mut idle: Vec<(usize, GpuStats)> = Vec::new();
    let mut order: Vec<String> = Vec::new();
    for (i, g) in accels.iter().enumerate() {
        let key = g
            .model
            .as_ref()
            .and_then(|m| m.model_name.clone())
            .filter(|n| !n.is_empty());
        match key {
            Some(name) => {
                if !by_model.contains_key(&name) {
                    order.push(name.clone());
                }
                by_model.entry(name).or_default().push((i, g.clone()));
            }
            None => idle.push((i, g.clone())),
        }
    }
    let mut clusters = Vec::new();
    for name in order {
        let gpus = by_model.remove(&name).unwrap_or_default();
        let tps_now = gpus
            .iter()
            .filter_map(|(_, g)| g.model.as_ref().and_then(|m| m.tps_current))
            .sum::<f32>();
        let tps_avg_v: Vec<f32> = gpus
            .iter()
            .filter_map(|(_, g)| g.model.as_ref().and_then(|m| m.tps_avg))
            .collect();
        let tps_avg = if tps_avg_v.is_empty() {
            0.0
        } else {
            tps_avg_v.iter().sum::<f32>() / tps_avg_v.len() as f32
        };
        let tps_peak = gpus
            .iter()
            .filter_map(|(_, g)| g.model.as_ref().and_then(|m| m.tps_best))
            .fold(0.0f32, f32::max);
        let ram_gb: f32 = gpus
            .iter()
            .filter_map(|(_, g)| g.model.as_ref().and_then(|m| m.ram_offloaded))
            .sum::<u64>() as f32
            / 1_073_741_824.0;
        let stripe = model_stripe_color(&name);
        clusters.push(Cluster {
            model: name,
            gpus,
            tps_now,
            tps_avg,
            tps_peak,
            ram_gb,
            stripe_color: stripe,
        });
    }
    if !idle.is_empty() {
        clusters.push(Cluster {
            model: "— idle —".to_string(),
            gpus: idle,
            tps_now: 0.0,
            tps_avg: 0.0,
            tps_peak: 0.0,
            ram_gb: 0.0,
            stripe_color: model_stripe_color("— idle —"),
        });
    }
    clusters
}

#[component]
fn NodeCard(node: NodeStatus, on_pick: EventHandler<String>) -> Element {
    let name = node.name.clone();
    let pick = name.clone();
    let up = node.reachable;
    let tel = node.telemetry.clone().unwrap_or_default();
    let os = tel.os.clone();
    let accels = tel.accelerators.clone();
    let n_gpu = accels.len();
    let cpu = tel.cpu_pct;
    let ram = pct(tel.memory_used_bytes, tel.memory_total_bytes);
    let rx = human_bps(tel.rx_bps);
    let tx = human_bps(tel.tx_bps);
    let up_txt = if up { "LINK" } else { "DOWN" };
    let err_txt = node.error.clone().unwrap_or_else(|| "none".to_string());
    // `class:` takes a value, not a `match` block, so decide it up front. A
    // node only goes "hot" if a red zone is actually breached right now.
    let card_cls = if !up {
        "card down"
    } else if cpu >= REDLINE * 100.0 || ram >= REDLINE * 100.0 {
        "card hot"
    } else {
        "card"
    };
    let clusters = group_clusters(&accels);

    rsx! {
        article {
            class: card_cls,
            onclick: move |_| on_pick.call(pick.clone()),
            div { class: "c-hd",
                div { class: "c-id",
                    span { class: "c-dot" }
                    h2 { "{name}" }
                }
                span { class: "c-state", "{up_txt}" }
            }
            div { class: "c-os", "{os}" }
            div { class: "c-mini",
                MiniBar { label: "CPU".to_string(), v: cpu }
                MiniBar { label: "RAM".to_string(), v: ram }
            }
            if up {
                div { class: "c-net",
                    span { class: "c-net-k", "RX" }
                    span { class: "c-net-v", "{rx}" }
                    span { class: "c-net-k", "TX" }
                    span { class: "c-net-v", "{tx}" }
                }
                div { class: "c-clusters",
                    for cluster in clusters.iter() {
                        div { class: if cluster.model == "— idle —" { "cluster idle" } else { "cluster" },
                            div { class: "racing-stripe", style: "background-color: {cluster.stripe_color}" }
                            if cluster.model != "— idle —" {
                                div { class: "cluster-banner",
                                    span { class: "cluster-model", "{cluster.model}" }
                                    span { class: "cluster-tps",
                                        "now ",
                                        span { class: "tps-v", "{cluster.tps_now:.1}" },
                                        "  avg ",
                                        span { class: "tps-v", "{cluster.tps_avg:.1}" },
                                        "  peak ",
                                        span { class: "tps-v", "{cluster.tps_peak:.1}" },
                                        " t/s"
                                    }
                                    if cluster.ram_gb > 0.0 {
                                        span { class: "cluster-ram", "↕ {cluster.ram_gb:.1} GB offload" }
                                    }
                                    span { class: "cluster-count", "{cluster.gpus.len()} GPU" }
                                }
                            }
                            div { class: "c-gpus",
                                for (i, g) in cluster.gpus.iter() {
                                    GpuRow { idx: *i, g: g.clone(), node_name: name.clone() }
                                }
                            }
                        }
                    }
                }
                div { class: "c-node-act",
                    a {
                        class: "c-test-all-link",
                        href: format!("/gpu-stress.html?target=node:{name}&autostart=true"),
                        target: "_blank",
                        onclick: move |e| e.stop_propagation(),
                        "⚡ RUN TEST ON ALL GPUS ({name})"
                    }
                }
                if n_gpu == 0 {
                    div { class: "c-none", "no accelerators reported" }
                }
            } else {
                div { class: "c-err",
                    "unreachable: ",
                    code { "{err_txt}" }
                }
            }
            div { class: "c-more", "DETAIL +" }
        }
    }
}

#[component]
fn GpuRow(idx: usize, g: GpuStats, node_name: String) -> Element {
    let v = pct(g.vram_used, g.vram_total);
    let t = g.temp_celsius.unwrap_or(0.0);
    let hot = t >= TEMP_HOT || v >= REDLINE * 100.0;
    let nm = g.name.clone();
    let vd = g.vendor.clone();
    let watts = g.power_watts.map(|p| format!("{p:.0}W")).unwrap_or_else(|| "--".into());
    let util = g.utilization;
    // Throttled: a model is loaded but generation is stalled vs its peak --
    // current TPS is nil/zero, or well below the observed best.
    let throttled = g.model.as_ref().map_or(false, |m| {
        let best = m.tps_best.unwrap_or(0.0);
        let cur = m.tps_current.unwrap_or(0.0);
        best > 0.0 && (cur == 0.0 || cur < best * 0.30)
    });
    let used_gb = gb(g.vram_used);
    let total_gb = gb(g.vram_total);
    // Hoisted out of rsx!: a nested string literal inside a text
    // interpolation can't be tokenised by the Rust lexer.
    let model_name = g
        .model
        .as_ref()
        .and_then(|m| m.model_name.clone())
        .unwrap_or_else(|| "—".to_string());
    rsx! {
        div { class: if hot { "gcard hot" } else { "gcard" },
            div { class: "gc-hd",
                span { class: "gc-ix", "{idx}" }
                span { class: "gc-nm", "{nm}" }
                span { class: "gc-u", "{util:.0}%" }
                if throttled { span { class: "gc-throttle blink", "THROTTLED" } }
            }
            div { class: "gc-vd", "{vd}" }
            if let Some(ref m) = g.model {
                div { class: "gc-model",
                    span { class: "gc-model-name", "{model_name}" }
                    span { class: "gc-model-engine", "{m.engine.clone().unwrap_or_default()}" }
                    if let Some(tps) = m.tps_current {
                        span { class: "gc-model-tps", "{tps:.1} t/s" }
                    }
                    if let Some(q) = m.quantization.clone() {
                        span { class: "gc-model-q", "{q}" }
                    }
                }
            }
            Bar { v, kind: "vram".to_string() }
            Bar { v: (t / TEMP_FULL) * 100.0, kind: "temp".to_string() }
            div { class: "gc-f",
                span { class: if t >= TEMP_HOT { "hot" } else { "" }, "{t:.1} C" }
                span { "{watts}" }
                span { "{used_gb:.1}/{total_gb:.1} GB" }
            }
            div { class: "gc-act",
                a {
                    class: "gc-test-link",
                    href: format!("/gpu-stress.html?target={node_name}:{idx}&autostart=true"),
                    target: "_blank",
                    onclick: move |e| e.stop_propagation(),
                    "⚡ TEST GPU {idx}"
                }
            }
        }
    }
}

/// A horizontal bar with the red band printed into its track, so the limit is
/// visible even when the value is low.
#[component]
fn Bar(v: f32, kind: String) -> Element {
    let w = v.clamp(0.0, 100.0);
    let over = v >= REDLINE * 100.0;
    rsx! {
        div { class: "bar {kind}",
            div { class: "bar-red" }
            div { class: if over { "bar-fill over" } else { "bar-fill" }, style: "width:{w:.1}%" }
            div { class: "bar-limit", style: "left:{REDLINE * 100.0:.1}%" }
        }
    }
}

#[component]
fn MiniBar(label: String, v: f32) -> Element {
    let w = v.clamp(0.0, 100.0);
    let over = v >= REDLINE * 100.0;
    rsx! {
        div { class: "mb",
            span { class: "mb-l", "{label}" }
            div { class: "mb-t",
                div { class: "mb-red" }
                div { class: if over { "mb-f over" } else { "mb-f" }, style: "width:{w:.1}%" }
            }
            span { class: if over { "mb-v over" } else { "mb-v" }, "{v:.0}%" }
        }
    }
}

/// Drill-down overlay. Everything the API returns for one node, including the
/// health flags and network counters that the cards deliberately omit.
#[component]
fn Detail(node: NodeStatus, fresh: Fresh, tsec: String, on_close: EventHandler<MouseEvent>) -> Element {
    let name = node.name.clone();
    let tel = node.telemetry.clone().unwrap_or_default();
    let err = node.error.clone().unwrap_or_else(|| "none".to_string());
    let h = node.health.clone().unwrap_or_default();
    let status = if h.status.is_empty() { "unknown".to_string() } else { h.status.clone() };
    let os = tel.os.clone();

    let cpu = tel.cpu_pct;
    let ram_pct = pct(tel.memory_used_bytes, tel.memory_total_bytes);
    let ram_used = gb(tel.memory_used_bytes);
    let ram_tot = gb(tel.memory_total_bytes);
    let rx = human_bps(tel.rx_bps);
    let tx = human_bps(tel.tx_bps);
    let n_gpu = tel.accelerators.len();

    rsx! {
        div { class: "ovl", onclick: move |e| on_close.call(e),
            div { class: "sheet", onclick: |e| e.stop_propagation(),
                div { class: "sheet-hd",
                    div {
                        div { class: "sheet-k", "NODE DETAIL" }
                        h2 { "{name}" }
                    }
                    button { class: "sheet-x", onclick: move |e| on_close.call(e), "CLOSE" }
                }
                div { class: "sheet-meta",
                    span { "T-SEC {tsec}" }
                    span { "{fresh_word(fresh)}" }
                    span { "{n_gpu} accelerators" }
                }
                if !node.reachable {
                    div { class: "sheet-down",
                        div { class: "sheet-down-t", "NODE UNREACHABLE" }
                        code { "{err}" }
                    }
                }
                section { class: "sheet-sec",
                    h3 { "PLATFORM" }
                    div { class: "kv",
                        span { class: "k", "os" } span { class: "v", "{os}" }
                        span { class: "k", "reported status" } span { class: "v", "{status}" }
                        span { class: "k", "cpu" } span { class: "v", "{cpu:.1}%" }
                        span { class: "k", "memory" } span { class: "v", "{ram_used:.1} / {ram_tot:.1} GB ({ram_pct:.0}%)" }
                        span { class: "k", "network in" } span { class: "v", "{rx}" }
                        span { class: "k", "network out" } span { class: "v", "{tx}" }
                    }
                    Bar { v: cpu, kind: "cpu".to_string() }
                    Bar { v: ram_pct, kind: "ram".to_string() }
                }
                section { class: "sheet-sec",
                    h3 { "HEALTH FLAGS" }
                    div { class: "flags",
                        Flag { label: "sysinfo collector".to_string(), good: h.sysinfo_ok, invert: false }
                        Flag { label: "all-smi".to_string(), good: h.all_smi_ok, invert: false }
                        Flag { label: "polaris fallback active".to_string(), good: h.polaris_fallback_active, invert: true }
                        Flag { label: "lemonade reachable".to_string(), good: h.lemonade_reachable, invert: false }
                        Flag { label: "bmc detected".to_string(), good: h.bmc_detected, invert: false }
                    }
                }
                section { class: "sheet-sec",
                    h3 { "ACCELERATORS" }
                    if n_gpu == 0 {
                        div { class: "sheet-none", "none reported" }
                    }
                    div { class: "c-node-act", style: "margin-bottom: 12px;",
                        a {
                            class: "c-test-all-link",
                            href: format!("/gpu-stress.html?target=node:{name}&autostart=true"),
                            target: "_blank",
                            onclick: move |e| e.stop_propagation(),
                            "⚡ RUN TEST ON ALL GPUS ({name})"
                        }
                    }
                    for (i, g) in tel.accelerators.iter().enumerate() {
                        GpuDetail { idx: i, g: g.clone(), node_name: name.clone() }
                    }
                }
            }
        }
    }
}

/// A health flag lamp. `invert` means the condition being TRUE is the bad
/// case (polaris fallback running), so the lamp reads red when set.
#[component]
fn Flag(label: String, good: bool, invert: bool) -> Element {
    let bad = if invert { good } else { !good };
    let cls = if bad { "flag bad" } else { "flag" };
    let word = if good { "YES" } else { "NO" };
    rsx! {
        div { class: cls,
            span { class: "flag-l" }
            span { class: "flag-t", "{label}" }
            span { class: "flag-v", "{word}" }
        }
    }
}

/// Full-width accelerator readout for the detail sheet: both bars, both
/// redlines, and the raw numbers.
#[component]
fn GpuDetail(idx: usize, g: GpuStats, node_name: String) -> Element {
    let v = pct(g.vram_used, g.vram_total);
    let t = g.temp_celsius.unwrap_or(0.0);
    let hot = t >= TEMP_HOT || v >= REDLINE * 100.0;
    let nm = g.name.clone();
    let vd = if g.vendor.is_empty() { "unknown".to_string() } else { g.vendor.clone() };
    let watts = g.power_watts.map(|p| format!("{p:.1} W")).unwrap_or_else(|| "not reported".into());
    let temps = g.temp_celsius.map(|p| format!("{p:.1} C")).unwrap_or_else(|| "not reported".into());
    let used_gb = gb(g.vram_used);
    let total_gb = gb(g.vram_total);
    rsx! {
        div { class: if hot { "gd hot" } else { "gd" },
            div { class: "gd-hd",
                span { class: "gd-ix", "GPU {idx}" }
                span { class: "gd-nm", "{nm}" }
                if hot { span { class: "gd-red blink", "REDLINE" } }
            }
            div { class: "kv",
                span { class: "k", "vendor" } span { class: "v", "{vd}" }
                span { class: "k", "utilisation" } span { class: "v", "{g.utilization:.1}%" }
                span { class: "k", "vram" } span { class: "v", "{used_gb:.2} / {total_gb:.2} GB ({v:.1}%)" }
                span { class: "k", "temperature" } span { class: "v", "{temps}" }
                span { class: "k", "power draw" } span { class: "v", "{watts}" }
            }
            Bar { v, kind: "vram".to_string() }
            Bar { v: (t / TEMP_FULL) * 100.0, kind: "temp".to_string() }
            div { class: "gd-act",
                a {
                    class: "gc-test-link",
                    href: format!("/gpu-stress.html?target={node_name}:{idx}&autostart=true"),
                    target: "_blank",
                    onclick: move |e| e.stop_propagation(),
                    "⚡ RUN STRESS TEST ON THIS GPU"
                }
            }
        }
    }
}

/// Footer carries the verdict, not just a clock. If the link is stale or dead
/// the whole bar goes red and says what it is showing instead.
#[component]
fn Footer(fresh: Fresh, tsec: String, why: String, stamp: u64) -> Element {
    let cls = match fresh {
        Fresh::Live => "ftr",
        Fresh::Stale => "ftr warn",
        Fresh::Boot => "ftr warn",
        Fresh::Lost => "ftr bad",
    };
    let msg = match fresh {
        Fresh::Boot => "AWAITING FIRST FRAME".to_string(),
        Fresh::Live => "LIVE // nauti-fleet :9122 via /dash/api/".to_string(),
        Fresh::Stale => format!("STALE // holding last frame, {}s old -- backend not answering", tsec),
        Fresh::Lost => format!("LINK LOST // nothing received for {}s", tsec),
    };
    rsx! {
        footer { class: cls,
            span { class: "f-led" }
            span { class: "f-msg", "{msg}" }
            if !why.is_empty() && fresh != Fresh::Live {
                span { class: "f-why", "({why})" }
            }
            span { class: "f-t", "payload {hhmmss(stamp)} UTC" }
            span { class: "f-sec", "T-SEC {tsec}" }
        }
    }
}
