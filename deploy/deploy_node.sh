#!/usr/bin/env bash
# Deploy nauti-daemon (the canonical fleet telemetry collector) to one node.
#
#   ./deploy_node.sh <node-name> <ssh-host> [lemonade-port]
#
# Idempotent: safe to re-run. Copies the prebuilt release binary + the unit,
# patches the node-name and lemonade port, then enables and starts it.
# Never fails the fleet if one node is unreachable.
set -uo pipefail

NODE_NAME="${1:?usage: deploy_node.sh <node-name> <ssh-host> [lemonade-port]}"
SSH_HOST="${2:?usage: deploy_node.sh <node-name> <ssh-host> [lemonade-port]}"
LEMONADE_PORT="${3:-13305}"

REPO="/codebase/repos/forge_lemon-cake/nauti-monitor"
BIN_SRC="${REPO}/target/release/nauti-daemon"
UNIT_SRC="${REPO}/deploy/nauti-monitor.service"
BIN_DST="${REPO}/target/release/nauti-daemon"
UNIT_DST="/etc/systemd/system/nauti-daemon.service"

echo "=== deploying nauti-daemon -> ${NODE_NAME} (${SSH_HOST}) ==="

# The unit must be readable by root to be copied into /etc/systemd/system.
tmp_unit="$(mktemp)"
sed -e "s/--node-name t440/--node-name ${NODE_NAME}/" \
    -e "s#http://127.0.0.1:13305#http://127.0.0.1:${LEMONADE_PORT}#" \
    "$UNIT_SRC" > "$tmp_unit"

# scp the unit to /tmp first (writable by us), then sudo into place.
scp -o BatchMode=yes -o ConnectTimeout=10 "$tmp_unit" "${SSH_HOST}:/tmp/nauti-daemon.service" || {
    echo "WARN: unit copy failed on ${NODE_NAME}"; rm -f "$tmp_unit"; exit 1; }
rm -f "$tmp_unit"

ssh -o BatchMode=yes -o ConnectTimeout=10 "$SSH_HOST" bash -s -- "$NODE_NAME" "$BIN_DST" "$UNIT_DST" <<'REMOTE'
set -uo pipefail
NODE_NAME="$1"; BIN_DST="$2"; UNIT_DST="$3"

# Ensure the binary is in place (prebuilt image should already have it; rebuild
# only if missing AND cargo is available).
if [ ! -x "$BIN_DST" ]; then
  echo "binary missing at ${BIN_DST}"
  if command -v cargo >/dev/null 2>&1; then
    echo "attempting cargo build -p nauti-daemon"
    (cd /codebase/repos/forge_lemon-cake/nauti-monitor && \
     cargo build --release -p nauti-daemon) || { echo "BUILD_FAILED"; exit 2; }
  else
    echo "NO_CARGO_AND_NO_BINARY"
    exit 3
  fi
fi

sudo -n install -m 0644 /tmp/nauti-daemon.service "$UNIT_DST" || { echo "UNIT_INSTALL_FAILED"; exit 4; }
rm -f /tmp/nauti-daemon.service
sudo -n systemctl daemon-reload
sudo -n systemctl enable nauti-daemon
sudo -n systemctl restart nauti-daemon
sleep 2
echo "active=$(systemctl is-active nauti-daemon)"
curl -s -m 5 http://127.0.0.1:9121/health || echo " (health check failed)"
REMOTE
rc=$?
echo "=== ${NODE_NAME} deploy rc=${rc} ==="
exit $rc