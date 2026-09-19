# Nauti-Monitor: Agent-First Telemetry Daemon & HUD

Lean, agent-first hardware and Lemonade telemetry daemon with a Model Context Protocol (MCP) surface and an embeddable Dioxus 0.6 WASM HUD panel for node `cesarops-3` (Arch Linux / Omarchy kernel).

## Key Architecture & Design Principles

- **Lean Core**: Uses `all-smi` (0.26.3, Apache-2.0) + `sysinfo` (0.32.0, MIT) instead of monolithic heavy dependencies.
- **AMD Polaris Resilient Fallback**: Scrapes multi-vendor GPUs via `all-smi`. If uninitialized or empty, falls back directly to sysfs for AMD Ellesmere cards (`/sys/class/drm/card*/device/mem_info_vram_*`, `gpu_busy_percent`, and `hwmon`).
- **Physical NIC Isolation**: Filters out virtual, loopback, container, and tunnel interfaces (`lo`, `veth*`, `br*`, `docker*`, `virbr*`, `tailscale*`, `tun*`, `tap*`, `wg*`), computing real delta byte transfer rates (`rx_bps`, `tx_bps`).
- **Lemonade LLM Telemetry**: Dynamically queries Lemonade's `/v1/models`, `/v1/health`, and stats. Never hardcodes model names or throughput if unreachable.
- **First-Class MCP Surface**: Supports standard JSON-RPC 2.0 over both `stdio` and HTTP (`POST /mcp`), allowing Omarchy AI agents and IDEs to call telemetry tools directly.
- **Localhost Security**: Default binding on `127.0.0.1:9100`.

---

## Workspace Layout

```
Nauti-Monitor/
├── Cargo.toml                 # Cargo workspace
├── README.md                  # Documentation
├── deploy/
│   └── nauti-monitor.service  # Systemd service unit for cesarops-3
├── src-daemon/                # Headless collector + HTTP + MCP
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs            # Axum HTTP server + background scraper loop + CLI
│       ├── types.rs           # UnifiedTelemetry, NicRate, GpuStats, LemonadeContext
│       ├── cache.rs           # Thread-safe ring buffer (snapshots + sliding window)
│       ├── scraper.rs         # sysinfo + all-smi + Polaris sysfs fallback
│       ├── lemonade.rs        # Real HTTP scraper for Lemonade
│       └── mcp.rs             # JSON-RPC 2.0 stdio & HTTP MCP engine
└── src-ui/                    # Embeddable Dioxus 0.6 WASM panel
    ├── Cargo.toml
    └── src/main.rs            # Lime-on-black terminal HUD component
```

---

## HTTP API Endpoints

All endpoints listen on `http://127.0.0.1:9100`:

| Endpoint | Method | Description |
| :--- | :--- | :--- |
| `/api/telemetry` | `GET` | Returns the latest `UnifiedTelemetry` snapshot. |
| `/api/telemetry/history` | `GET` | Returns historical samples. Optional query: `?seconds=60`. |
| `/health` | `GET` | Daemon status (`sysinfo_ok`, `all_smi_ok`, `polaris_fallback_active`, `lemonade_reachable`). |
| `/mcp` | `POST` | JSON-RPC 2.0 MCP interface. |

---

## MCP Tools for Agents

Omarchy agents can connect via stdio (`nauti-daemon --mcp-stdio`) or HTTP (`POST /mcp`).

### Available Tools

1. `fetch_live_telemetry`:
   * Fetch current CPU, RAM, physical NIC rates, GPU cards, and Lemonade context.
2. `fetch_history`:
   * Fetch historical telemetry over the ring buffer (optional argument `seconds: u64`).
3. `list_physical_nics`:
   * Returns physical network interfaces and current byte rates (`rx_bps`, `tx_bps`).
4. `get_gpu_thermal`:
   * Returns current temperatures, power draw, and utilization for all accelerators.

### Example Tool Call via HTTP

```bash
curl -X POST http://127.0.0.1:9100/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/call",
    "params": {
      "name": "fetch_live_telemetry",
      "arguments": {}
    }
  }'
```

---

## Running on `cesarops-3`

```bash
# Build release binary
cargo build --release -p nauti-daemon

# Run daemon
./target/release/nauti-daemon --bind 127.0.0.1:9100 --node-name cesarops-3

# Install systemd service
sudo cp deploy/nauti-monitor.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now nauti-monitor
```
