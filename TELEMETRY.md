# Fleet Telemetry — Single Source of Truth

**Read this before asking "what GPUs does node X have?" or writing anything that
reads hardware state.**

As of 2026-09-27 the fleet had **five** different ways to read telemetry, and
they disagreed with each other. There is now **one**.

## THE RULE

> **Never run `nvidia-smi`, `rocm-smi`, `all-smi snapshot`, or query HQ `:7700`
> for GPU/VRAM/capacity answers. Ask `nauti-fleet` (`:9122`).**

If you need hardware facts, use one of these, in order of preference:

```bash
# 1. Fleet rollup — the answer to "how much VRAM does the fleet have?"
curl -s localhost:9122/api/fleet/summary

# 2. Per-node detail — "what does node X look like?"
curl -s localhost:9122/api/fleet/nodes
curl -s localhost:9122/api/fleet/telemetry

# 3. Single node (run ON that node; it binds loopback only)
curl -s localhost:9121/api/telemetry
curl -s localhost:9121/health

# 4. Web-facing passthrough (:8767, same data, legacy shapes kept for the site)
curl -s localhost:8767/api/gpus
curl -s localhost:8767/api/fleet/summary
```

## Architecture

```
  t440                                   c2 / c3 / c4om
  ┌──────────────────────────┐           ┌────────────────────┐
  │ nauti-daemon   :9121     │           │ nauti-daemon :9121 │  loopback ONLY
  │ nauti-fleet    :9122  ◄──┼─ SSH ─────┤  (all-smi crate +  │
  │   (aggregator)           │  tunnels  │   Polaris sysfs)   │
  │ proxy.py      :8767      │           └────────────────────┘
  │ forge-hq      :7700  ◄──────── IDENTITY ONLY (not telemetry)
  └──────────────────────────┘
```

- **`nauti-daemon`** (per node) — all-Rust. `all-smi` 0.26 + `sysinfo` + AMD
  Polaris sysfs fallback. 1s scrape. Loopback only.
- **`nauti-fleet`** (t440 only) — aggregates every node over SSH tunnels, serves
  the fleet view + an MCP surface. Loopback only.
- **Tunnels** — `nauti-tunnel-<node>` units. SSH `-L` forwards the remote
  daemon to `127.0.0.1:1922x` on t440. This is why the daemons can stay
  loopback-only and still be reachable by the aggregator.

**Port map (t440):** `9121` daemon · `9122` fleet aggregator · `8767` web proxy
· `7700` Forge HQ (identity) · `9100` Forge · `9090` Cockpit (**do not** use
for all-smi api — it collides) · `8001` **unused** (Lemonade is `13305`).
## Ports on each node

| Node | LAN | Tailscale | Daemon |
|---|---|---|---|
| t440 | 192.168.4.29, 10.44.0.10, 10.0.10.61 | 100.72.182.77 | 9121 |
| cesarops2 | 192.168.4.24 | 100.109.55.22 | 9121 |
| cesarops3 | 192.168.4.31 | 100.76.255.40 | 9121 |
| cesarops4om | *(moved — was 192.168.4.39, now 192.168.7.214)* | 100.65.31.48 | 9121 |

**Prefer Tailscale IPs for SSH.** The LAN addressing is being reworked; during
the transition LAN routes vanish and Tailscale keeps working. `cesarops4om`
already moved subnet, which broke LAN-only tunnels once.

## What was retired and why

| Retired | Reason |
|---|---|
| `fleet-worker` 4-layer GPU walk (`nvidia-smi`→`rocm-smi`→`/sys/class/drm`→`lspci`) | Reported BMC/Matrox display adapters as GPUs at 0 MB. c2 claimed 5 accelerators for 3 real cards; t440 claimed 4 for 2. Now delegates to `nauti-daemon`. |
| `discover_models()` `gpu_indices: vec![0, 1]` | Hardcoded for every model on every node. Now derived from real per-PID VRAM attribution; returns `[]` when unproven. |
| `proxy.py` `subprocess.run(["all-smi","snapshot"])` per request | Re-spawned a binary on every API call (15s timeout each). Now reads the aggregator. |
| `proxy.py` `temp_shutdown: 95`, `temp_slowdown: 90`, `active: used > 2 GiB` | Invented constants. Removed — utilisation and VRAM now come from the hardware. |
| `nauti-nodes/src/metrics/mod.rs` Prometheus exporter | Never deployed; no binary, no unit, nothing served `/metrics/prometheus`. The fleet webpage never worked. |
| `/var/www/cesarops/fleet-metrics.html` + `fleet-metrics.js` | Polled `/ops/fleet-live`, which **404s and exists in no source tree**. Dead. |
| `nauti` binary `gpus` subcommand | Reports **0 MB VRAM** on every card while the daemon reports 10.9 GB correctly. Broken adapter — do not use for capacity answers. |
| `all-smi api` on :9090 | Compiled default collides with Cockpit. Not used. |

## Retained on purpose

- **`forge-hq` `:7700`** — kept deliberately. It owns node **identity and
  enrollment**, and `scripts/lib/fleet_resolve.py` **generates `~/.ssh/config`
  from `GET /v1/fleet`**. Killing it costs you the SSH aliases, including the
  Tailscale fallback into nodes that have left the LAN. It is **not** a
  telemetry source — for hardware, use `:9122`.

## Deploying to a new node

```bash
cd /codebase/repos/forge_lemon-cake/nauti-monitor

# 1. daemon (loopback-only, per node)
./deploy/deploy_node.sh <name> <ssh-host> [lemonade-port]
#    Lemonade port is 13305 on t440, 13306 on c2/c3.

# 2. tunnel from t440 so the aggregator can reach it
./deploy/tunnel.sh <name> <ssh-host> <local-port>   # e.g. 19224
sudo systemctl enable --now nauti-tunnel-<name>

# 3. register with the aggregator — edit the unit, not the code
sudo $EDITOR /etc/systemd/system/nauti-fleet.service   # NAUTI_FLEET_NODES
sudo systemctl restart nauti-fleet
```

## Security notes

- `:9121`, `:9122`, `:8767` are **loopback only**.
- `:9106` was bound `0.0.0.0` (plain `python3 -m http.server`) and served
  `/var/lib/nauti`, which includes `standing-reboot-order.json` — an **armed,
  self-executing** `ForceRestart` order against t440's iLO — plus
  `nauti_iommu_repair.py`, the script that acts on it, and `leases/`+`store/`.
  **Now bound `127.0.0.1`.** Re-check it after any network reconfiguration; a
  subnet change previously reset it to a LAN address.
- `cesarops-nauti-repair.timer` fires **every 30s** with `--repair --ask`.

## MCP surface (for agents)

`POST http://127.0.0.1:9122/mcp` — JSON-RPC 2.0.
Tools: `fleet_telemetry`, `fleet_health`, `fleet_summary`, `fleet_vram_leaks`
(accelerators with VRAM pinned by a process — useful before triggering rescue).

```bash
curl -s -X POST localhost:9122/mcp -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call",
       "params":{"name":"fleet_vram_leaks","arguments":{}}}'
```

## Honesty contract

`nauti-fleet` reports `reachable:false` + `error` per node and never fabricates
a value for a node it cannot reach. `FleetSummary.gpus_total` counts only what
was actually observed. An empty `gpu_indices` means "not proven" — it is not a
claim of GPU 0. Do not paper over either with a fallback constant.
