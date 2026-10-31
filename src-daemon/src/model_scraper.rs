use crate::types::{GpuStats, ModelServingInfo};
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, warn};

pub struct ModelScraper {
    client: Client,
    endpoints: Vec<ModelEndpoint>,
}

#[derive(Debug, Clone)]
pub struct ModelEndpoint {
    pub base_url: String,
    pub gpu_indices: Vec<u32>,
    pub engine_hint: Option<String>,
}

impl ModelScraper {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_millis(2000))
            .build()
            .unwrap_or_default();

        let endpoints = Self::default_endpoints();

        Self { client, endpoints }
    }

    fn default_endpoints() -> Vec<ModelEndpoint> {
        vec![
            // t440 (local)
            ModelEndpoint { base_url: "http://127.0.0.1:8102/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("llama-server".to_string()) },
            ModelEndpoint { base_url: "http://127.0.0.1:8105/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("llama-server-vulkan".to_string()) },
            ModelEndpoint { base_url: "http://127.0.0.1:8085/v1".to_string(), gpu_indices: vec![0,1,2,3], engine_hint: Some("poly-strata".to_string()) },
            ModelEndpoint { base_url: "http://127.0.0.1:8310/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("omarchy-agent".to_string()) },
            ModelEndpoint { base_url: "http://127.0.0.1:8481/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("poly-strata".to_string()) },
            // c2 (10.44.0.10)
            ModelEndpoint { base_url: "http://10.44.0.10:8102/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("llama-server".to_string()) },
            ModelEndpoint { base_url: "http://10.44.0.10:8105/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("llama-server-vulkan".to_string()) },
            ModelEndpoint { base_url: "http://10.44.0.10:8085/v1".to_string(), gpu_indices: vec![0,1,2,3], engine_hint: Some("poly-strata".to_string()) },
            ModelEndpoint { base_url: "http://10.44.0.10:8310/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("omarchy-agent".to_string()) },
            ModelEndpoint { base_url: "http://10.44.0.10:8481/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("poly-strata".to_string()) },
            // c3 (10.44.0.13)
            ModelEndpoint { base_url: "http://10.44.0.13:8106/v1".to_string(), gpu_indices: vec![0,1], engine_hint: Some("llama-server".to_string()) },
            ModelEndpoint { base_url: "http://10.44.0.13:8310/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("omarchy-agent".to_string()) },
            // c4 (10.44.0.12)
            ModelEndpoint { base_url: "http://10.44.0.12:8101/v1".to_string(), gpu_indices: vec![0,1,2], engine_hint: Some("llama-server".to_string()) },
            ModelEndpoint { base_url: "http://10.44.0.12:8105/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("llama-server-vulkan".to_string()) },
            ModelEndpoint { base_url: "http://10.44.0.12:8085/v1".to_string(), gpu_indices: vec![0,1,2,3], engine_hint: Some("poly-strata".to_string()) },
            ModelEndpoint { base_url: "http://10.44.0.12:8310/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("omarchy-agent".to_string()) },
            // c4om (10.44.0.14)
            ModelEndpoint { base_url: "http://10.44.0.14:8102/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("llama-server".to_string()) },
            ModelEndpoint { base_url: "http://10.44.0.14:8310/v1".to_string(), gpu_indices: vec![0], engine_hint: Some("omarchy-agent".to_string()) },
        ]
    }

    pub async fn scrape(&self) -> HashMap<u32, Vec<ModelServingInfo>> {
        let mut results: HashMap<u32, Vec<ModelServingInfo>> = HashMap::new();

        for endpoint in &self.endpoints {
            match self.scrape_endpoint(endpoint).await {
                Ok(models) => {
                    for gpu_idx in &endpoint.gpu_indices {
                        results.entry(*gpu_idx).or_default().extend(models.clone());
                    }
                }
                Err(e) => {
                    debug!("Failed to scrape {}: {}", endpoint.base_url, e);
                }
            }
        }

        results
    }

    async fn scrape_endpoint(&self, endpoint: &ModelEndpoint) -> Result<Vec<ModelServingInfo>, anyhow::Error> {
        let models_url = format!("{}/models", endpoint.base_url.trim_end_matches('/'));

        let resp = self.client.get(&models_url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("HTTP {}", resp.status()));
        }

        let json: Value = resp.json().await?;
        let mut models = Vec::new();

        if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
            for item in data {
                if let Some(model_info) = self.parse_model_item(item, endpoint).await {
                    models.push(model_info);
                }
            }
        } else if let Some(models_arr) = json.get("models").and_then(|m| m.as_array()) {
            for item in models_arr {
                if let Some(model_info) = self.parse_model_item(item, endpoint).await {
                    models.push(model_info);
                }
            }
        }

        if let Ok(stats) = self.scrape_stats(endpoint).await {
            for model in &mut models {
                model.tps_current = stats.tps_current.or(model.tps_current);
                model.tps_avg = stats.tps_avg.or(model.tps_avg);
                model.tps_best = stats.tps_best.or(model.tps_best);
                model.context_length = stats.context_length.or(model.context_length);
                model.quantization = stats.quantization.clone().or(model.quantization.clone());
                model.vram_allocated = stats.vram_allocated.or(model.vram_allocated);
                model.ram_offloaded = stats.ram_offloaded.or(model.ram_offloaded);
                model.status = stats.status.clone().or(model.status.clone());
            }
        }

        Ok(models)
    }

    async fn parse_model_item(&self, item: &Value, endpoint: &ModelEndpoint) -> Option<ModelServingInfo> {
        let model_name = item.get("id").and_then(|v| v.as_str())
            .or_else(|| item.get("name").and_then(|v| v.as_str()))
            .or_else(|| item.get("model").and_then(|v| v.as_str()))?
            .to_string();

        let mut info = ModelServingInfo {
            model_name: Some(model_name),
            engine: endpoint.engine_hint.clone(),
            endpoint: Some(endpoint.base_url.clone()),
            primary_gpu_index: endpoint.gpu_indices.first().copied(),
            gpu_indices: Some(endpoint.gpu_indices.clone()),
            last_updated_ms: Some(chrono::Utc::now().timestamp_millis() as u64),
            ..Default::default()
        };

        info.quantization = Self::extract_quantization(&info.model_name.as_ref().unwrap());

        if let Some(owned_by) = item.get("owned_by").and_then(|v| v.as_str()) {
            if info.engine.is_none() {
                info.engine = Some(owned_by.to_string());
            }
        }

        if let Some(max_len) = item.get("max_model_len").and_then(|v| v.as_u64()) {
            info.context_length = Some(max_len as u32);
        } else if let Some(ctx) = item.get("context_length").and_then(|v| v.as_u64()) {
            info.context_length = Some(ctx as u32);
        }

        Some(info)
    }

    fn extract_quantization(model_name: &str) -> Option<String> {
        let patterns = [
            "Q2_K", "Q3_K", "Q4_K", "Q5_K", "Q6_K", "Q8_0",
            "Q2_K_S", "Q3_K_S", "Q4_K_S", "Q5_K_S",
            "IQ1_", "IQ2_", "IQ3_", "IQ4_",
            "FP16", "FP8", "BF16",
            "EXL2", "EXL3",
            "GPTQ", "AWQ",
        ];
        for pat in patterns {
            if model_name.contains(pat) {
                return Some(pat.to_string());
            }
        }
        None
    }

    async fn scrape_stats(&self, endpoint: &ModelEndpoint) -> Result<ModelServingInfo, anyhow::Error> {
        let mut info = ModelServingInfo::default();

        let health_url = format!("{}/health", endpoint.base_url.trim_end_matches('/'));
        if let Ok(resp) = self.client.get(&health_url).send().await {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<Value>().await {
                    if let Some(backend) = json.get("backend").and_then(|v| v.as_str()) {
                        info.engine = Some(backend.to_string());
                    }
                    if let Some(status) = json.get("status").and_then(|v| v.as_str()) {
                        info.status = Some(status.to_string());
                    }
                }
            }
        }

        for stats_path in ["/api/v1/stats", "/v1/stats", "/stats"] {
            let stats_url = format!("{}{}", endpoint.base_url.trim_end_matches('/'), stats_path);
            if let Ok(resp) = self.client.get(&stats_url).send().await {
                if resp.status().is_success() {
                    if let Ok(json) = resp.json::<Value>().await {
                        if let Some(tps) = json.get("tokens_per_sec").and_then(|v| v.as_f64()) {
                            info.tps_current = Some(tps as f32);
                        }
                        if let Some(avg) = json.get("tokens_per_sec_avg").and_then(|v| v.as_f64()) {
                            info.tps_avg = Some(avg as f32);
                        } else if let Some(avg) = json.get("avg_tokens_per_sec").and_then(|v| v.as_f64()) {
                            info.tps_avg = Some(avg as f32);
                        }
                        if let Some(best) = json.get("tokens_per_sec_best").and_then(|v| v.as_f64()) {
                            info.tps_best = Some(best as f32);
                        } else if let Some(best) = json.get("peak_tokens_per_sec").and_then(|v| v.as_f64()) {
                            info.tps_best = Some(best as f32);
                        }
                        if let Some(ctx) = json.get("context_size").and_then(|v| v.as_u64()) {
                            info.context_length = Some(ctx as u32);
                        }
                        if let Some(vram) = json.get("vram_used").and_then(|v| v.as_u64()) {
                            info.vram_allocated = Some(vram);
                        }
                        if let Some(ram) = json.get("ram_offloaded").and_then(|v| v.as_u64()) {
                            info.ram_offloaded = Some(ram);
                        }
                        if let Some(quant) = json.get("quantization").and_then(|v| v.as_str()) {
                            info.quantization = Some(quant.to_string());
                        }
                        break;
                    }
                }
            }
        }

        Ok(info)
    }
}

impl Default for ModelScraper {
    fn default() -> Self {
        Self::new()
    }
}