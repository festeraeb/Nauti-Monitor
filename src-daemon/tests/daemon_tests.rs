use chrono::Utc;
use nauti_daemon::cache::TelemetryCache;
use nauti_daemon::mcp::{JsonRpcRequest, McpHandler};
use nauti_daemon::types::{GpuStats, LemonadeContext, NicRate, UnifiedTelemetry};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_cache_ring_buffer() {
    let cache = TelemetryCache::new(3);

    for i in 0..5 {
        let mut sample = UnifiedTelemetry::default();
        sample.cpu_pct = i as f32;
        sample.timestamp = Utc::now();
        cache.update(sample).await;
    }

    let snap = cache.snapshot().await;
    assert_eq!(snap.cpu_pct, 4.0);

    let hist = cache.history(None).await;
    assert_eq!(hist.len(), 3);
    assert_eq!(hist[0].cpu_pct, 2.0);
    assert_eq!(hist[1].cpu_pct, 3.0);
    assert_eq!(hist[2].cpu_pct, 4.0);
}

#[tokio::test]
async fn test_mcp_handler_tools() {
    let cache = Arc::new(TelemetryCache::new(10));

    let mut sample = UnifiedTelemetry::default();
    sample.node_name = "cesarops-3".to_string();
    sample.cpu_pct = 42.5;

    let mut nics = HashMap::new();
    nics.insert("enp0s25".to_string(), NicRate { rx_bps: 1048576, tx_bps: 524288 });
    sample.physical_nics = nics;

    sample.accelerators.push(GpuStats {
        name: "AMD Radeon RX 580".to_string(),
        vendor: "AMD".to_string(),
        utilization: 15.0,
        vram_used: 1073741824,
        vram_total: 8589934592,
        temp_celsius: Some(48.0),
        power_watts: Some(65.0),
        model: None,
    });

    sample.lemonade = LemonadeContext {
        healthy: true,
        active_models: vec!["Qwen3-14B-GGUF".to_string()],
        downloaded_models: vec!["Qwen3-14B-GGUF".to_string()],
        tokens_per_sec: Some(24.5),
        context_tokens: Some(8192),
        backend: Some("Vulkan".to_string()),
        raw_notes: None,
    };

    cache.update(sample).await;

    let handler = McpHandler::new(cache);

    // 1. Initialize
    let init_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "initialize".to_string(),
        params: None,
    };
    let init_resp = handler.handle_request(init_req).await;
    assert!(init_resp.error.is_none());
    assert_eq!(init_resp.result.unwrap()["serverInfo"]["name"], "nauti-monitor");

    // 2. Tools list
    let list_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "tools/list".to_string(),
        params: None,
    };
    let list_resp = handler.handle_request(list_req).await;
    let tools = list_resp.result.unwrap()["tools"].as_array().unwrap().len();
    assert_eq!(tools, 4);

    // 3. fetch_live_telemetry tool call
    let call_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "fetch_live_telemetry",
            "arguments": {}
        })),
    };
    let call_resp = handler.handle_request(call_req).await;
    assert!(call_resp.error.is_none());
    let text = call_resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
    assert!(text.contains("cesarops-3"));
    assert!(text.contains("Qwen3-14B-GGUF"));
    assert!(text.contains("enp0s25"));

    // 4. get_gpu_thermal tool call
    let thermal_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "get_gpu_thermal",
            "arguments": {}
        })),
    };
    let thermal_resp = handler.handle_request(thermal_req).await;
    let thermal_text = thermal_resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
    assert!(thermal_text.contains("AMD Radeon RX 580"));
    assert!(thermal_text.contains("48.0"));

    // 5. Unknown tool error handling (no panic)
    let bad_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(5)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "non_existent_tool",
            "arguments": {}
        })),
    };
    let bad_resp = handler.handle_request(bad_req).await;
    assert!(bad_resp.error.is_some());
    assert_eq!(bad_resp.error.unwrap().code, -32602);
}
