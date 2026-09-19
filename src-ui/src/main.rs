use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use gloo_net::http::Request;
use gloo_timers::future::TimeoutFuture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UnifiedTelemetry {
    pub timestamp: DateTime<Utc>,
    pub node_name: String,
    pub os: String,
    pub cpu_pct: f32,
    pub ram_used_bytes: u64,
    pub ram_total_bytes: u64,
    pub physical_nics: HashMap<String, NicRate>,
    pub accelerators: Vec<GpuStats>,
    pub lemonade: LemonadeContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NicRate {
    pub rx_bps: u64,
    pub tx_bps: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct GpuStats {
    pub name: String,
    pub vendor: String,
    pub utilization: f32,
    pub vram_used: u64,
    pub vram_total: u64,
    pub temp_celsius: Option<f32>,
    pub power_watts: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct LemonadeContext {
    pub healthy: bool,
    pub active_models: Vec<String>,
    pub tokens_per_sec: Option<f32>,
    pub context_tokens: Option<u32>,
    pub backend: Option<String>,
    pub raw_notes: Option<String>,
}

fn format_bytes(bytes: u64) -> String {
    let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    if gib >= 1.0 {
        format!("{:.2} GiB", gib)
    } else {
        let mib = bytes as f64 / (1024.0 * 1024.0);
        format!("{:.1} MiB", mib)
    }
}

fn format_rate(bytes_per_sec: u64) -> String {
    let kb = bytes_per_sec as f64 / 1024.0;
    if kb >= 1024.0 {
        format!("{:.2} MB/s", kb / 1024.0)
    } else {
        format!("{:.1} KB/s", kb)
    }
}

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut telemetry = use_signal(UnifiedTelemetry::default);
    let mut connection_error = use_signal(|| Option::<String>::None);

    use_future(move || async move {
        loop {
            match Request::get("/api/telemetry").send().await {
                Ok(resp) => {
                    if resp.status() == 200 {
                        if let Ok(data) = resp.json::<UnifiedTelemetry>().await {
                            telemetry.set(data);
                            connection_error.set(None);
                        }
                    } else {
                        connection_error.set(Some(format!("HTTP Error {}", resp.status())));
                    }
                }
                Err(e) => {
                    connection_error.set(Some(format!("Connection offline: {}", e)));
                }
            }
            TimeoutFuture::new(1000).await;
        }
    });

    let data = telemetry.read();
    let ram_pct = if data.ram_total_bytes > 0 {
        (data.ram_used_bytes as f64 / data.ram_total_bytes as f64) * 100.0
    } else {
        0.0
    };

    rsx! {
        style {
            {r#"
                * { box-sizing: border-box; margin: 0; padding: 0; }
                body {
                    background-color: #0A0D0B;
                    color: #00FF66;
                    font-family: 'JetBrains Mono', 'Fira Code', monospace;
                    padding: 1.5rem;
                    line-height: 1.4;
                }
                .hud-container {
                    max-width: 1200px;
                    margin: 0 auto;
                    border: 1px solid #00AA44;
                    box-shadow: 0 0 20px rgba(0, 255, 102, 0.15);
                    background: rgba(10, 15, 12, 0.95);
                    padding: 1.5rem;
                }
                .header-bar {
                    display: flex;
                    justify-content: space-between;
                    align-items: center;
                    border-bottom: 2px solid #00FF66;
                    padding-bottom: 1rem;
                    margin-bottom: 1.5rem;
                }
                .title { font-size: 1.4rem; font-weight: 800; letter-spacing: 2px; text-transform: uppercase; }
                .live-badge {
                    display: inline-flex;
                    align-items: center;
                    gap: 0.5rem;
                    background: #003311;
                    padding: 0.3rem 0.8rem;
                    border: 1px solid #00FF66;
                    border-radius: 4px;
                    font-size: 0.85rem;
                }
                .pulse-dot {
                    width: 10px;
                    height: 10px;
                    background: #00FF66;
                    border-radius: 50%;
                    box-shadow: 0 0 8px #00FF66;
                }
                .grid {
                    display: grid;
                    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
                    gap: 1.5rem;
                    margin-bottom: 1.5rem;
                }
                .card {
                    background: #0D1410;
                    border: 1px solid #006622;
                    padding: 1.2rem;
                    border-radius: 2px;
                }
                .card h3 {
                    color: #FFFFFF;
                    font-size: 1rem;
                    margin-bottom: 0.8rem;
                    border-bottom: 1px dashed #006622;
                    padding-bottom: 0.4rem;
                    display: flex;
                    justify-content: space-between;
                }
                .bar-container {
                    background: #00220A;
                    height: 14px;
                    border: 1px solid #00AA44;
                    margin: 0.4rem 0 0.8rem 0;
                    position: relative;
                }
                .bar-fill {
                    background: #00FF66;
                    height: 100%;
                    transition: width 0.3s ease;
                }
                .metric-row {
                    display: flex;
                    justify-content: space-between;
                    margin-bottom: 0.4rem;
                    font-size: 0.9rem;
                }
                .metric-label { color: #88CC99; }
                .metric-value { font-weight: 700; }
                .tag {
                    display: inline-block;
                    background: #00441A;
                    border: 1px solid #00FF66;
                    padding: 0.2rem 0.5rem;
                    margin: 0.2rem 0.3rem 0.2rem 0;
                    font-size: 0.8rem;
                    color: #FFFFFF;
                }
                .error-banner {
                    background: #330000;
                    color: #FF4444;
                    border: 1px solid #FF4444;
                    padding: 0.8rem;
                    margin-bottom: 1rem;
                    font-weight: bold;
                }
            "#}
        }

        div { class: "hud-container",
            // Header
            div { class: "header-bar",
                div {
                    div { class: "title", "NAUTI-MONITOR // CESAROPS-3 TELEMETRY HUD" }
                    div { style: "color: #88CC99; font-size: 0.8rem; margin-top: 0.2rem;",
                        "NODE: {data.node_name} | OS: {data.os}"
                    }
                }
                div { class: "live-badge",
                    div { class: "pulse-dot" }
                    span { "LIVE AGENT STREAM" }
                }
            }

            // Error Banner if unreachable
            if let Some(ref err) = *connection_error.read() {
                div { class: "error-banner", "WARNING: {err}" }
            }

            // Metrics Grid
            div { class: "grid",
                // CPU & RAM Card
                div { class: "card",
                    h3 { span { "SYSTEM COMPUTE & MEMORY" } span { "{data.cpu_pct:.1}% CPU" } }
                    div { class: "metric-row",
                        span { class: "metric-label", "CPU Utilization" }
                        span { class: "metric-value", "{data.cpu_pct:.1}%" }
                    }
                    div { class: "bar-container",
                        div { class: "bar-fill", style: "width: {data.cpu_pct.min(100.0):.1}%;" }
                    }

                    div { class: "metric-row",
                        span { class: "metric-label", "System RAM" }
                        span { class: "metric-value", "{format_bytes(data.ram_used_bytes)} / {format_bytes(data.ram_total_bytes)}" }
                    }
                    div { class: "bar-container",
                        div { class: "bar-fill", style: "width: {ram_pct.min(100.0):.1}%; background: #33FF88;" }
                    }
                }

                // Physical NICs Card
                div { class: "card",
                    h3 { span { "PHYSICAL NETWORK ADAPTERS" } span { "{data.physical_nics.len()} Online" } }
                    if data.physical_nics.is_empty() {
                        div { style: "color: #669977; font-size: 0.85rem;", "No physical NICs detected" }
                    }
                    for (iface, rate) in data.physical_nics.iter() {
                        div { style: "border: 1px solid #00441A; padding: 0.6rem; margin-bottom: 0.5rem; background: #08100A;",
                            div { class: "metric-row",
                                span { style: "font-weight: bold; color: #FFFFFF;", "{iface}" }
                                span { class: "tag", "PHYSICAL" }
                            }
                            div { class: "metric-row",
                                span { class: "metric-label", "Download (RX)" }
                                span { class: "metric-value", "{format_rate(rate.rx_bps)}" }
                            }
                            div { class: "metric-row",
                                span { class: "metric-label", "Upload (TX)" }
                                span { class: "metric-value", "{format_rate(rate.tx_bps)}" }
                            }
                        }
                    }
                }

                // Lemonade LLM Context Card
                div { class: "card",
                    h3 {
                        span { "LEMONADE VULKAN LLM" }
                        if data.lemonade.healthy {
                            span { style: "color: #00FF66;", "[ONLINE]" }
                        } else {
                            span { style: "color: #FF5555;", "[OFFLINE]" }
                        }
                    }
                    div { class: "metric-row",
                        span { class: "metric-label", "Status" }
                        span { class: "metric-value", if data.lemonade.healthy { "Serving Active Models" } else { "Unreachable / Idle" } }
                    }
                    if let Some(ref backend) = data.lemonade.backend {
                        div { class: "metric-row",
                            span { class: "metric-label", "Engine Backend" }
                            span { class: "metric-value", "{backend}" }
                        }
                    }
                    if let Some(tps) = data.lemonade.tokens_per_sec {
                        div { class: "metric-row",
                            span { class: "metric-label", "Inference Rate" }
                            span { class: "metric-value", "{tps:.1} tok/s" }
                        }
                    }
                    div { style: "margin-top: 0.6rem;",
                        div { class: "metric-label", style: "margin-bottom: 0.3rem;", "Loaded Models:" }
                        if data.lemonade.active_models.is_empty() {
                            span { style: "color: #669977; font-size: 0.85rem;", "None loaded" }
                        } else {
                            for model in &data.lemonade.active_models {
                                span { class: "tag", "{model}" }
                            }
                        }
                    }
                }
            }

            // Accelerators / GPUs Card
            div { class: "card",
                h3 { span { "ACCELERATOR MATRIX (ALL-SMI & POLARIS SYSFS)" } span { "{data.accelerators.len()} Detected" } }
                if data.accelerators.is_empty() {
                    div { style: "color: #669977; font-size: 0.85rem;", "No GPU accelerators detected" }
                }
                div { style: "display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: 1rem;",
                    for gpu in data.accelerators.clone() {
                        GpuCard { gpu }
                    }
                }
            }

            // Footer
            div { style: "margin-top: 1.5rem; text-align: center; color: #447755; font-size: 0.8rem;",
                "OMARCHY AGENT TELEMETRY // BIND: 127.0.0.1:9100 // REAL BYTE RATES"
            }
        }
    }
}

#[component]
fn GpuCard(gpu: GpuStats) -> Element {
    let temp_display = gpu.temp_celsius.map(|t| format!("{:.1} °C", t)).unwrap_or_else(|| "N/A".to_string());
    let power_display = gpu.power_watts.map(|p| format!("{:.1} W", p)).unwrap_or_else(|| "N/A".to_string());
    let vram_pct = if gpu.vram_total > 0 {
        (gpu.vram_used as f64 / gpu.vram_total as f64 * 100.0).min(100.0)
    } else {
        0.0
    };

    rsx! {
        div { style: "border: 1px solid #00441A; padding: 0.8rem; background: #08100A;",
            div { style: "font-weight: bold; color: #FFFFFF; margin-bottom: 0.4rem;", "{gpu.name}" }
            div { class: "metric-row",
                span { class: "metric-label", "Vendor / Family" }
                span { class: "metric-value", "{gpu.vendor}" }
            }
            div { class: "metric-row",
                span { class: "metric-label", "GPU Core Load" }
                span { class: "metric-value", "{gpu.utilization:.1}%" }
            }
            div { class: "bar-container",
                div { class: "bar-fill", style: "width: {gpu.utilization.min(100.0):.1}%;" }
            }
            div { class: "metric-row",
                span { class: "metric-label", "VRAM Usage" }
                span { class: "metric-value", "{format_bytes(gpu.vram_used)} / {format_bytes(gpu.vram_total)}" }
            }
            div { class: "bar-container",
                div {
                    class: "bar-fill",
                    style: "width: {vram_pct:.1}%; background: #44FFAA;"
                }
            }
            div { class: "metric-row",
                span { class: "metric-label", "Thermal" }
                span { class: "metric-value", "{temp_display}" }
            }
            div { class: "metric-row",
                span { class: "metric-label", "Power Draw" }
                span { class: "metric-value", "{power_display}" }
            }
        }
    }
}
