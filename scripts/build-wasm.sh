#!/usr/bin/env bash
# Build the CodonSplice WASM package and assemble the npm `pkg/` directory.
#
# Requires: wasm-pack (cargo install wasm-pack) and the wasm32 target
# (rustup target add wasm32-unknown-unknown).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "▶ building codonsplice-wasm (wasm-pack, target web)…"
wasm-pack build crates/codonsplice-wasm \
  --target web \
  --out-dir "$ROOT/pkg/wasm" \
  --out-name codonsplice_wasm \
  --release

echo "▶ copying framework wrappers + convenience entry points…"
node scripts/post-wasm-build.js

echo "✓ pkg/ ready — publish with: (cd pkg && npm publish --access public)"
