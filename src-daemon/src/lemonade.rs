use crate::types::LemonadeContext;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, warn};

pub struct LemonadeScraper {
    client: Client,
    base_url: String,
}

/// Append a reason to the raw_notes field (and log it). Missing data stays None.
fn note(ctx: &mut LemonadeContext, reason: String) {
    warn!("lemonade: {}", reason);
    match &mut ctx.raw_notes {
        Some(n) => {
            n.push_str("; ");
            n.push_str(&reason);
        }
        None => ctx.raw_notes = Some(reason),
    }
}

/// Parse GET /api/v1/health (Lemonade 11.9.0 shape). `healthy` is set only from
/// `status == "ok"`. The loaded model comes from `model_loaded` / `all_models_loaded[]`;
/// backend and context size come from that entry's `recipe_options`.
pub fn apply_health(ctx: &mut LemonadeContext, json: &Value) {
    match json.get("status").and_then(|s| s.as_str()) {
        Some("ok") => ctx.healthy = true,
        Some(other) => note(ctx, format!("health status is {:?}, not \"ok\"", other)),
        None => note(ctx, "health response has no string `status`".to_string()),
    }

    let loaded = match json.get("all_models_loaded").and_then(|a| a.as_array()) {
        Some(a) => a,
        None => {
            note(ctx, "health response has no `all_models_loaded` array".to_string());
            return;
        }
    };
    for m in loaded {
        if let Some(name) = m.get("model_name").and_then(|n| n.as_str()) {
            ctx.active_models.push(name.to_string());
        }
    }
    if loaded.is_empty() {
        // Nothing loaded is a real state, not an error; backend/context stay None.
        debug!("lemonade: no model loaded");
        return;
    }

    // Pick the entry for `model_loaded`; if that key is absent or unmatched, only
    // use an entry when exactly one model is loaded (otherwise it would be a guess).
    let wanted = json.get("model_loaded").and_then(|m| m.as_str());
    let entry = match wanted {
        Some(w) => loaded.iter().find(|m| m.get("model_name").and_then(|n| n.as_str()) == Some(w)),
        None if loaded.len() == 1 => loaded.first(),
        None => None,
    };
    let entry = match entry {
        Some(e) => e,
        None => {
            note(ctx, "cannot match `model_loaded` to an `all_models_loaded` entry; backend/context left null".to_string());
            return;
        }
    };
    let opts = entry.get("recipe_options");
    match opts.and_then(|o| o.get("llamacpp_backend")).and_then(|b| b.as_str()) {
        Some(b) => ctx.backend = Some(b.to_string()),
        None => note(ctx, "loaded model has no recipe_options.llamacpp_backend; backend null".to_string()),
    }
    match opts.and_then(|o| o.get("ctx_size")).and_then(|c| c.as_u64()).map(u32::try_from) {
        Some(Ok(c)) => ctx.context_tokens = Some(c),
        _ => note(ctx, "loaded model has no valid recipe_options.ctx_size; context_tokens null".to_string()),
    }
}

/// Parse GET /api/v1/stats. Key is `tokens_per_second`; `tokens_per_sec` is accepted
/// as a fallback. Non-finite or negative values are rejected (left None).
pub fn apply_stats(ctx: &mut LemonadeContext, json: &Value) {
    let v = json
        .get("tokens_per_second")
        .or_else(|| json.get("tokens_per_sec"))
        .and_then(|t| t.as_f64());
    match v {
        Some(t) if t.is_finite() && t >= 0.0 => ctx.tokens_per_sec = Some(t as f32),
        Some(t) => note(ctx, format!("stats tokens_per_second invalid ({}); left null", t)),
        None => note(ctx, "stats has no numeric tokens_per_second/tokens_per_sec; left null".to_string()),
    }
}

/// Parse GET /v1/models: ids of downloaded models (NOT necessarily loaded).
pub fn apply_models(ctx: &mut LemonadeContext, json: &Value) {
    match json.get("data").and_then(|d| d.as_array()) {
        Some(data) => {
            for item in data {
                if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                    ctx.downloaded_models.push(id.to_string());
                }
            }
        }
        None => note(ctx, "models response has no `data` array".to_string()),
    }
}

impl LemonadeScraper {
    pub fn new(base_url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_millis(1500))
            .build()
            .unwrap_or_default();
        Self { client, base_url }
    }

    async fn get_json(&self, path: &str) -> Result<Value, String> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        let resp = self.client.get(&url).send().await.map_err(|e| format!("{} unreachable: {}", url, e))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} from {}", resp.status(), url));
        }
        resp.json::<Value>().await.map_err(|e| format!("{} invalid JSON: {}", url, e))
    }

    pub async fn scrape(&self) -> LemonadeContext {
        let mut ctx = LemonadeContext::default();

        match self.get_json("/api/v1/health").await {
            Ok(j) => apply_health(&mut ctx, &j),
            Err(e) => note(&mut ctx, e),
        }
        match self.get_json("/api/v1/stats").await {
            Ok(j) => apply_stats(&mut ctx, &j),
            Err(e) => note(&mut ctx, e),
        }
        match self.get_json("/v1/models").await {
            Ok(j) => apply_models(&mut ctx, &j),
            Err(e) => note(&mut ctx, e),
        }
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures recorded from live Lemonade 11.9.0 (cesarops3:13305, 2026-10-21).
    const HEALTH: &str = include_str!("../tests/fixtures/lemonade_health.json");
    const STATS: &str = include_str!("../tests/fixtures/lemonade_stats.json");
    const MODELS: &str = include_str!("../tests/fixtures/lemonade_models.json");

    fn j(s: &str) -> Value {
        serde_json::from_str(s).expect("fixture parses")
    }

    #[test]
    fn live_fixtures_populate_all_fields() {
        let mut ctx = LemonadeContext::default();
        apply_health(&mut ctx, &j(HEALTH));
        apply_stats(&mut ctx, &j(STATS));
        apply_models(&mut ctx, &j(MODELS));
        assert!(ctx.healthy);
        assert_eq!(ctx.active_models, vec!["Qwen3-14B-GGUF"]);
        assert_eq!(ctx.backend.as_deref(), Some("cpu"));
        assert_eq!(ctx.context_tokens, Some(4096));
        assert!((ctx.tokens_per_sec.unwrap() - 2.415881).abs() < 1e-4);
        assert_eq!(ctx.downloaded_models, vec!["Qwen3-0.6B-GGUF", "Qwen3-14B-GGUF"]);
        assert!(ctx.raw_notes.is_none());
    }

    #[test]
    fn tokens_per_sec_fallback_key() {
        let mut ctx = LemonadeContext::default();
        apply_stats(&mut ctx, &serde_json::json!({"tokens_per_sec": 7.5}));
        assert_eq!(ctx.tokens_per_sec, Some(7.5));
    }

    #[test]
    fn missing_or_invalid_stays_null_with_reason() {
        let mut ctx = LemonadeContext::default();
        apply_stats(&mut ctx, &serde_json::json!({"tokens_per_second": "fast"}));
        apply_stats(&mut ctx, &serde_json::json!({"tokens_per_second": -1.0}));
        apply_health(&mut ctx, &serde_json::json!({"status": "ok", "all_models_loaded": [{"model_name": "m"}], "model_loaded": "m"}));
        assert!(ctx.healthy);
        assert_eq!(ctx.tokens_per_sec, None);
        assert_eq!(ctx.backend, None);
        assert_eq!(ctx.context_tokens, None);
        let n = ctx.raw_notes.unwrap();
        assert!(n.contains("tokens_per_second") && n.contains("llamacpp_backend") && n.contains("ctx_size"));
    }

    #[test]
    fn unhealthy_status_not_ok() {
        let mut ctx = LemonadeContext::default();
        apply_health(&mut ctx, &serde_json::json!({"status": "starting", "all_models_loaded": []}));
        assert!(!ctx.healthy);
        assert!(ctx.active_models.is_empty());
        assert!(ctx.raw_notes.unwrap().contains("starting"));
    }
}
