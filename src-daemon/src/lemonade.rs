use crate::types::LemonadeContext;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tracing::debug;

pub struct LemonadeScraper {
    client: Client,
    base_url: String,
}

impl LemonadeScraper {
    pub fn new(base_url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_millis(1500))
            .build()
            .unwrap_or_default();
        Self { client, base_url }
    }

    pub async fn scrape(&self) -> LemonadeContext {
        let mut ctx = LemonadeContext {
            healthy: false,
            active_models: Vec::new(),
            tokens_per_sec: None,
            context_tokens: None,
            backend: None,
            raw_notes: None,
        };

        // 1. Probe models endpoint (/v1/models)
        let models_url = format!("{}/v1/models", self.base_url.trim_end_matches('/'));
        match self.client.get(&models_url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    ctx.healthy = true;
                    if let Ok(json) = resp.json::<Value>().await {
                        // Standard OpenAI/vLLM format: {"data": [{"id": "model_name"}, ...]}
                        if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
                            for item in data {
                                if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                                    ctx.active_models.push(id.to_string());
                                }
                            }
                        } else if let Some(models) = json.get("models").and_then(|m| m.as_array()) {
                            // Alternative format: {"models": ["m1", "m2"]}
                            for m in models {
                                if let Some(id) = m.as_str() {
                                    ctx.active_models.push(id.to_string());
                                } else if let Some(id) = m.get("name").and_then(|n| n.as_str()) {
                                    ctx.active_models.push(id.to_string());
                                }
                            }
                        }
                    }
                } else {
                    ctx.raw_notes = Some(format!("HTTP {} from {}", resp.status(), models_url));
                }
            }
            Err(e) => {
                debug!("Failed to query Lemonade models at {}: {}", models_url, e);
                ctx.raw_notes = Some(format!("Unreachable: {}", e));
            }
        }

        // 2. Probe health / stats endpoint if available
        let health_url = format!("{}/v1/health", self.base_url.trim_end_matches('/'));
        if let Ok(resp) = self.client.get(&health_url).send().await {
            if resp.status().is_success() {
                ctx.healthy = true;
                if let Ok(json) = resp.json::<Value>().await {
                    if let Some(backend) = json.get("backend").and_then(|b| b.as_str()) {
                        ctx.backend = Some(backend.to_string());
                    }
                }
            }
        }

        // 3. Optional stats endpoint (/api/v1/stats or /v1/stats)
        let stats_url = format!("{}/api/v1/stats", self.base_url.trim_end_matches('/'));
        if let Ok(resp) = self.client.get(&stats_url).send().await {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<Value>().await {
                    if let Some(tps) = json.get("tokens_per_sec").and_then(|t| t.as_f64()) {
                        ctx.tokens_per_sec = Some(tps as f32);
                    }
                    if let Some(ctx_len) = json.get("context_size").and_then(|c| c.as_u64()) {
                        ctx.context_tokens = Some(ctx_len as u32);
                    }
                    if let Some(b) = json.get("backend").and_then(|b| b.as_str()) {
                        ctx.backend = Some(b.to_string());
                    }
                }
            }
        }

        ctx
    }
}
