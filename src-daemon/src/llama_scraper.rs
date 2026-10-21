use crate::types::ModelServingInfo;
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::debug;

/// A runtime-discovered llama-server instance.
#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    pub pid: u32,
    pub port: u16,
    pub base_url: String,
    /// GPU indices this server's process has open (via /proc/<pid>/fd -> /dev/nvidiaN)
    pub gpu_indices: Vec<u32>,
}

/// Per-slot sample used to compute decoded-token deltas for passive TPS.
#[derive(Debug, Clone, Default)]
struct SlotSample {
    slot_id: u32,
    n_gen: u64,
    is_processing: bool,
}

/// Scrapes llama-server instances discovered at runtime.
pub struct LlamaScraper {
    client: Client,
    prev_samples: HashMap<u16, HashMap<u32, SlotSample>>,
    prev_instants: HashMap<u16, Instant>,
    discovered: HashMap<u32, DiscoveredServer>,
    discovered_at: Option<Instant>,
}

impl LlamaScraper {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_millis(1500))
            .build()
            .unwrap_or_default();

        Self {
            client,
            prev_samples: HashMap::new(),
            prev_instants: HashMap::new(),
            discovered: HashMap::new(),
            discovered_at: None,
        }
    }

    fn discover_servers(&mut self) {
        if let Some(inst) = self.discovered_at {
            if inst.elapsed().as_secs() < 10 {
                return;
            }
        }

        let mut found = HashMap::new();

        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let pid_str = entry.file_name().to_string_lossy().to_string();
                let pid: u32 = match pid_str.parse() {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                let cmdline_path = format!("/proc/{}/cmdline", pid);
                let cmdline = match fs::read_to_string(&cmdline_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                if !cmdline.contains("llama-server") {
                    continue;
                }

                let port = match extract_port(&cmdline) {
                    Some(p) => p,
                    None => continue,
                };

                let base_url = format!("http://127.0.0.1:{}/v1", port);
                let gpu_indices = attribute_gpus(pid);

                found.insert(
                    pid,
                    DiscoveredServer {
                        pid,
                        port,
                        base_url,
                        gpu_indices: gpu_indices.clone(),
                    },
                );

                debug!("discovered llama-server pid={} port={} gpus={:?}", pid, port, gpu_indices);
            }
        }

        self.discovered = found;
        self.discovered_at = Some(Instant::now());
    }

    async fn scrape_server(&mut self, server: &DiscoveredServer) -> Option<ModelServingInfo> {
        let props_url = format!("{}/props", server.base_url.trim_end_matches("/v1"));
        let slots_url = format!("{}/slots", server.base_url.trim_end_matches("/v1"));

        // --- /props: model metadata ---
        let (model_name, _model_path, quantization, context_length) = self.fetch_props(&props_url).await;

        // --- /slots: per-slot decode counters for TPS ---
        let now = Instant::now();
        let elapsed_secs = self
            .prev_instants
            .get(&server.port)
            .map(|prev| (now - *prev).as_secs_f64())
            .filter(|e| *e > 0.0);

        let slots: Value = match self.client.get(&slots_url).send().await {
            Ok(resp) if resp.status().is_success() => resp.json().await.ok()?,
            _ => {
                // Server unreachable — report TPS as None (unavailable).
                return Some(ModelServingInfo {
                    model_name,
                    engine: Some("llama-server".to_string()),
                    endpoint: Some(server.base_url.clone()),
                    quantization,
                    context_length,
                    gpu_indices: Some(server.gpu_indices.clone()),
                    primary_gpu_index: server.gpu_indices.first().copied(),
                    status: Some("unreachable".to_string()),
                    last_updated_ms: Some(now_ms()),
                    ..Default::default()
                });
            }
        };

        let prev_samples = self.prev_samples.remove(&server.port).unwrap_or_default();
        let mut new_samples = HashMap::new();

        let mut tps_values = Vec::new();
        let mut any_processing = false;

        if let Some(arr) = slots.as_array() {
            for slot in arr {
                let slot_id = slot.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let is_processing = slot.get("is_processing").and_then(|v| v.as_bool()).unwrap_or(false);
                let n_gen = slot
                    .get("next_token")
                    .and_then(|arr| arr.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|t| t.get("n_decoded"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                if is_processing {
                    any_processing = true;
                }

                new_samples.insert(slot_id, SlotSample { slot_id, n_gen, is_processing });

                // Compute TPS delta if we have a previous sample.
                let tps = if let Some(elapsed) = elapsed_secs {
                    if let Some(prev) = prev_samples.get(&slot_id) {
                        let delta = n_gen.saturating_sub(prev.n_gen);
                        let tps = delta as f64 / elapsed as f64;
                        if tps > 0.0 {
                            Some(tps as f32)
                        } else if !is_processing {
                            // Idle slot with no new tokens.
                            Some(0.0)
                        } else {
                            None
                        }
                    } else if !is_processing {
                        // First sample, slot idle.
                        Some(0.0)
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(t) = tps {
                    tps_values.push(t);
                }
            }
        }

        self.prev_samples.insert(server.port, new_samples);
        self.prev_instants.insert(server.port, now);

        let tps_current = if tps_values.is_empty() {
            if any_processing {
                None
            } else {
                Some(0.0)
            }
        } else {
            // Sum across slots (a multi-slot server decoding on several slots
            // reports aggregate throughput).
            Some(tps_values.iter().sum::<f32>())
        };

        let tps_best = tps_current;

        Some(ModelServingInfo {
            model_name,
            engine: Some("llama-server".to_string()),
            endpoint: Some(server.base_url.clone()),
            quantization,
            context_length,
            tps_current,
            tps_avg: None,
            tps_best,
            gpu_indices: Some(server.gpu_indices.clone()),
            primary_gpu_index: server.gpu_indices.first().copied(),
            status: Some(if any_processing { "processing" } else { "idle" }.to_string()),
            last_updated_ms: Some(now_ms()),
            ..Default::default()
        })
    }

    async fn fetch_props(&self, props_url: &str) -> (Option<String>, Option<String>, Option<String>, Option<u32>) {
        let result = match self.client.get(props_url).send().await {
            Ok(resp) if resp.status().is_success() => resp.json::<Value>().await.ok(),
            _ => None,
        };

        let json = match result {
            Some(v) => v,
            None => return (None, None, None, None),
        };

        let model_name = json.get("model_alias").and_then(|v| v.as_str()).map(|s| s.to_string());
        let _model_path = json.get("model_path").and_then(|v| v.as_str()).map(|s| s.to_string());
        let quantization = json
            .get("model_ftype")
            .and_then(|v| v.as_str())
            .map(|s| s.split(" - ").next().unwrap_or(s).to_string());
        let context_length = json
            .get("default_generation_settings")
            .and_then(|v| v.get("params"))
            .and_then(|v| v.get("n_ctx"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        (model_name, None, quantization, context_length)
    }

    pub async fn scrape(&mut self) -> HashMap<u32, Vec<ModelServingInfo>> {
        self.discover_servers();

        let mut result: HashMap<u32, Vec<ModelServingInfo>> = HashMap::new();

        let servers: Vec<DiscoveredServer> = self.discovered.values().cloned().collect();

        for server in servers {
            let info = self.scrape_server(&server).await;
            let Some(info) = info else { continue };

            let gpu_indices = info.gpu_indices.clone().unwrap_or_default();
            let targets: Vec<u32> = if gpu_indices.is_empty() {
                vec![u32::MAX]
            } else {
                gpu_indices
            };

            for gpu_idx in targets {
                result.entry(gpu_idx).or_default().push(info.clone());
            }
        }

        result
    }
}

/// Extract the --port value from a llama-server cmdline string.
fn extract_port(cmdline: &str) -> Option<u16> {
    let flat = cmdline.replace('\0', " ");
    let mut tokens = flat.split_whitespace();

    while let Some(tok) = tokens.next() {
        if tok == "--port" {
            if let Some(val) = tokens.next() {
                return val.parse::<u16>().ok();
            }
        } else if let Some(rest) = tok.strip_prefix("--port=") {
            return rest.parse::<u16>().ok();
        }
    }
    None
}

/// Attribute a process to NVIDIA GPUs by reading /proc/<pid>/fd and collecting
/// symlink targets that match /dev/nvidiaN. Returns the sorted unique GPU indices.
fn attribute_gpus(pid: u32) -> Vec<u32> {
    let fd_dir = format!("/proc/{}/fd", pid);
    let mut gpus: Vec<u32> = Vec::new();

    if let Ok(entries) = fs::read_dir(&fd_dir) {
        for entry in entries.flatten() {
            let link = entry.path();
            if let Ok(target) = fs::read_link(&link) {
                let target_str = target.to_string_lossy().to_string();
                if let Some(rest) = target_str.strip_prefix("/dev/nvidia") {
                    if let Ok(idx) = rest.parse::<u32>() {
                        if !gpus.contains(&idx) {
                            gpus.push(idx);
                        }
                    }
                }
            }
        }
    }

    gpus.sort_unstable();
    gpus
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Default for LlamaScraper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_port() {
        assert_eq!(
            extract_port("--host 0.0.0.0 --port 8103 --alias foo"),
            Some(8103)
        );
        assert_eq!(extract_port("--port=8104"), Some(8104));
        assert_eq!(extract_port("--host 0.0.0.0 --alias foo"), None);
        assert_eq!(
            extract_port("/home/user/llama-server -m model.gguf --port 8103 --port 8104"),
            Some(8103)
        );
    }

    #[test]
    fn test_attribute_gpus_nonexistent_pid() {
        assert!(attribute_gpus(999999).is_empty());
    }
}
