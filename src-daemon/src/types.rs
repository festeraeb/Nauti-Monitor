use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Mail server operational health telemetry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MailHealth {
    pub smtp_online: bool,
    pub queue_count: u32,
    pub storage_used_bytes: u64,
    pub storage_total_bytes: u64,
    pub last_handshake_ms: u32,
}

/// Server-grade BMC / IPMI / Redfish / chassis hardware health.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BmcHealth {
    pub bmc_detected: bool,
    pub bmc_type: String,
    pub ambient_temp_c: Option<f32>,
    pub cpu_temp_c: Option<f32>,
    pub fan_rpm_avg: Option<u32>,
    pub power_watts: Option<f32>,
    pub psu_status: String,
}

/// Instantaneous network transfer rate for physical network adapters.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NicRate {
    pub rx_bps: u64,
    pub tx_bps: u64,
}

/// Model serving telemetry per GPU.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    /// GPU index this model is primarily using (for multi-GPU models)
    pub primary_gpu_index: Option<u32>,
    /// All GPU indices this model spans
    pub gpu_indices: Option<Vec<u32>>,
    /// Last updated timestamp (unix ms)
    pub last_updated_ms: Option<u64>,
    /// Health/status of the model server
    pub status: Option<String>,
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
    /// Model serving information for this GPU
    #[serde(default)]
    pub model: Option<ModelServingInfo>,
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

/// Unified telemetry payload reported by the Nauti-Monitor daemon.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UnifiedTelemetry {
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub timestamp_unix: i64,
    pub node_name: String,
    pub os: String,

    pub cpu_pct: f32,
    #[serde(default)]
    pub cpu_usage: f32,

    pub ram_used_bytes: u64,
    #[serde(default)]
    pub memory_used_bytes: u64,

    pub ram_total_bytes: u64,
    #[serde(default)]
    pub memory_total_bytes: u64,

    #[serde(default)]
    pub rx_bps: f64,
    #[serde(default)]
    pub tx_bps: f64,

    pub physical_nics: HashMap<String, NicRate>,
    pub accelerators: Vec<GpuStats>,
    #[serde(default)]
    pub gpu_stats: Vec<GpuStats>,

    pub lemonade: LemonadeContext,
    #[serde(default)]
    pub lemonade_healthy: bool,

    #[serde(default)]
    pub mail_health: MailHealth,

    #[serde(default)]
    pub bmc_health: Option<BmcHealth>,
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
    pub mail_reachable: bool,
    pub bmc_detected: bool,
}
