use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unified telemetry payload reported by the Nauti-Monitor daemon.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

/// Instantaneous network transfer rate for physical network adapters.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NicRate {
    pub rx_bps: u64,
    pub tx_bps: u64,
}

/// Hardware telemetry metrics for an accelerator (GPU/NPU).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GpuStats {
    pub name: String,
    pub vendor: String,
    pub utilization: f32,
    pub vram_used: u64,
    pub vram_total: u64,
    pub temp_celsius: Option<f32>,
    pub power_watts: Option<f32>,
}

/// Runtime context from the local or cluster Lemonade LLM engine.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LemonadeContext {
    pub healthy: bool,
    pub active_models: Vec<String>,
    pub tokens_per_sec: Option<f32>,
    pub context_tokens: Option<u32>,
    pub backend: Option<String>,
    pub raw_notes: Option<String>,
}

/// Overall daemon health status reported at /health.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonHealth {
    pub status: String,
    pub uptime_secs: u64,
    pub sysinfo_ok: bool,
    pub all_smi_ok: bool,
    pub polaris_fallback_active: bool,
    pub lemonade_reachable: bool,
}
