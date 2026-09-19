use crate::cache::TelemetryCache;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, info};

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub struct McpHandler {
    cache: Arc<TelemetryCache>,
}

impl McpHandler {
    pub fn new(cache: Arc<TelemetryCache>) -> Self {
        Self { cache }
    }

    /// Process a single JSON-RPC 2.0 request and return a response.
    pub async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        if req.jsonrpc != "2.0" {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32600,
                    message: "Invalid Request: jsonrpc must be '2.0'".to_string(),
                    data: None,
                }),
            };
        }

        let id = req.id;
        match req.method.as_str() {
            "initialize" => {
                let result = json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        }
                    },
                    "serverInfo": {
                        "name": "nauti-monitor",
                        "version": "0.1.0"
                    }
                });
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(result),
                    error: None,
                }
            }
            "notifications/initialized" => {
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: None,
                    result: Some(json!({})),
                    error: None,
                }
            }
            "tools/list" => {
                let tools = json!({
                    "tools": [
                        {
                            "name": "fetch_live_telemetry",
                            "description": "Fetch the latest real-time hardware, network, GPU, and Lemonade telemetry snapshot.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {}
                            }
                        },
                        {
                            "name": "fetch_history",
                            "description": "Fetch historical telemetry samples over the sliding window (optionally filtered by last N seconds).",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "seconds": {
                                        "type": "integer",
                                        "description": "Number of recent seconds to return (default: all available history)"
                                    }
                                }
                            }
                        },
                        {
                            "name": "list_physical_nics",
                            "description": "List filtered physical network interface cards and their instantaneous byte rates.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {}
                            }
                        },
                        {
                            "name": "get_gpu_thermal",
                            "description": "Get current temperature, power consumption, and utilization for all detected accelerators (all-smi or Polaris sysfs).",
                            "inputSchema": {
                                "type": "object",
                                "properties": {}
                            }
                        }
                    ]
                });
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(tools),
                    error: None,
                }
            }
            "tools/call" => {
                let params = req.params.unwrap_or(Value::Null);
                let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                match self.call_tool(tool_name, arguments).await {
                    Ok(val) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: Some(json!({
                            "content": [
                                {
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string())
                                }
                            ]
                        })),
                        error: None,
                    },
                    Err((code, msg)) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: None,
                        error: Some(JsonRpcError {
                            code,
                            message: msg,
                            data: None,
                        }),
                    },
                }
            }
            _ => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", req.method),
                    data: None,
                }),
            },
        }
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<Value, (i32, String)> {
        match name {
            "fetch_live_telemetry" => {
                let snap = self.cache.snapshot().await;
                Ok(json!(snap))
            }
            "fetch_history" => {
                let seconds = args.get("seconds").and_then(|s| s.as_u64());
                let hist = self.cache.history(seconds).await;
                Ok(json!(hist))
            }
            "list_physical_nics" => {
                let snap = self.cache.snapshot().await;
                Ok(json!(snap.physical_nics))
            }
            "get_gpu_thermal" => {
                let snap = self.cache.snapshot().await;
                let thermals: Vec<Value> = snap
                    .accelerators
                    .iter()
                    .map(|g| {
                        json!({
                            "name": g.name,
                            "vendor": g.vendor,
                            "utilization": g.utilization,
                            "temp_celsius": g.temp_celsius,
                            "power_watts": g.power_watts,
                            "vram_used_bytes": g.vram_used,
                            "vram_total_bytes": g.vram_total,
                        })
                    })
                    .collect();
                Ok(json!(thermals))
            }
            _ => Err((-32602, format!("Unknown tool name: '{}'", name))),
        }
    }

    /// Run stdio JSON-RPC loop for agent / CLI integration.
    pub async fn run_stdio_loop(self: Arc<Self>) {
        info!("Starting MCP stdio loop for agent interaction");
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin).lines();

        while let Ok(Some(line)) = reader.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonRpcRequest>(line) {
                Ok(req) => {
                    let resp = self.handle_request(req).await;
                    if let Ok(resp_str) = serde_json::to_string(&resp) {
                        let _ = stdout.write_all(resp_str.as_bytes()).await;
                        let _ = stdout.write_all(b"\n").await;
                        let _ = stdout.flush().await;
                    }
                }
                Err(e) => {
                    debug!("Malformed JSON-RPC request on stdin: {}", e);
                    let err_resp = JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: None,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32700,
                            message: format!("Parse error: {}", e),
                            data: None,
                        }),
                    };
                    if let Ok(resp_str) = serde_json::to_string(&err_resp) {
                        let _ = stdout.write_all(resp_str.as_bytes()).await;
                        let _ = stdout.write_all(b"\n").await;
                        let _ = stdout.flush().await;
                    }
                }
            }
        }
    }
}
