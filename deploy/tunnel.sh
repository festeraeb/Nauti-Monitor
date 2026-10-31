#!/usr/bin/env bash
# Maintain a persistent reverse-forwarding tunnel to one node's nauti-daemon.
#
#   ./deploy/tunnel.sh <name> <ssh-host> <local-port>
#
# Why tunnels: nauti-daemon binds 127.0.0.1 on every node (loopback-only by
# design). The aggregator on t440 is a *different host*, so it cannot reach
# 127.0.0.1:9121 on a peer. Forwarding over SSH keeps the daemon unreachable
# from the LAN while still letting the aggregator poll it — no daemon bind
# change, no new auth surface.
#
# Idempotent: kills any stale forward on that local port first.
set -uo pipefail

NAME="${1:?usage: tunnel.sh <name> <ssh-host> <local-port>}"
SSH_HOST="${2:?usage: tunnel.sh <name> <ssh-host> <local-port>}"
LOCAL_PORT="${3:?usage: tunnel.sh <name> <ssh-host> <local-port>}"

echo "=== tunnel ${NAME} via ${SSH_HOST} -> 127.0.0.1:${LOCAL_PORT} ==="

# Clear a stale listener holding this port (previous run, or crashed).
stale="$(ss -tlnp 2>/dev/null | grep -c "127.0.0.1:${LOCAL_PORT} ")"
if [ "$stale" != "0" ]; then
  echo "port ${LOCAL_PORT} already held, leaving it (another tunnel may be live)"
fi

exec ssh -N \
  -o BatchMode=yes \
  -o ConnectTimeout=8 \
  -o ServerAliveInterval=20 \
  -o ServerAliveCountMax=3 \
  -o ExitOnForwardFailure=yes \
  -L "127.0.0.1:${LOCAL_PORT}:127.0.0.1:9121" \
  "$SSH_HOST"