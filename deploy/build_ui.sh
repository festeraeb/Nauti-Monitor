#!/usr/bin/env bash
# Build nauti-ui (Dioxus WASM) and publish it into the nauti-fleet web root.
#
# Why a script and not ad-hoc cargo calls: the target dir is redirected via
# CARGO_TARGET_DIR, the wasm-bindgen out-name must match the import in
# index.html (/nfui/nauti_ui.js), and a half-applied deploy leaves the fleet
# serving a stale bundle. Fail loudly instead.
set -euo pipefail

UI_DIR="${UI_DIR:-/codebase/repos/forge_lemon-cake/nauti-monitor/src-ui}"
WEB_ROOT="${WEB_ROOT:-/var/www/nauti-fleet}"
OUT_NAME="nauti_ui"

cd "$UI_DIR"

echo "==> building nauti-ui (wasm32-unknown-unknown, release)"
cargo build --release --target wasm32-unknown-unknown

# Respect CARGO_TARGET_DIR if the environment sets one, else fall back to ./target.
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
  TARGET_ROOT="$CARGO_TARGET_DIR"
else
  TARGET_ROOT="$(cargo metadata --format-version 1 --no-deps \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
fi

REL="$TARGET_ROOT/wasm32-unknown-unknown/release"
# The wasm is named after the *package* (nauti-ui), not the crate id.
WASM=""
for cand in "$REL/nauti_ui.wasm" "$REL/nauti-ui.wasm"; do
  [ -f "$cand" ] && { WASM="$cand"; break; }
done
[ -n "$WASM" ] || { echo "FATAL: no wasm in $REL (looked for nauti_ui/nauti-ui)" >&2; exit 1; }

echo "==> wasm-bindgen -> $WEB_ROOT/nfui"
mkdir -p "$WEB_ROOT/nfui"
wasm-bindgen --target web --no-typescript \
  --out-dir "$WEB_ROOT/nfui" --out-name "$OUT_NAME" "$WASM"

echo "==> publishing index.html"
cp index.html "$WEB_ROOT/index.html"

echo "==> deployed:"
ls -la "$WEB_ROOT/index.html" "$WEB_ROOT/nfui/$OUT_NAME.js" "$WASM"

# Fail if the mount point the Rust app needs is missing; without it the app
# mounts to <body> and paints a blank page with no visible error.
grep -q 'id="main"' "$WEB_ROOT/index.html" \
  || { echo "FATAL: index.html has no #main mount point" >&2; exit 1; }

echo "OK"
