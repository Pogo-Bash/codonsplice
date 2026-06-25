#!/usr/bin/env bash
# Assemble the @codonsplice/cli npm packages from prebuilt `splice` binaries.
#
# Produces, under pkg/:
#   cli/                  — platform-agnostic main package (postinstall resolver)
#   cli-linux-x64/        — Linux x86_64 binary
#   cli-linux-arm64/      — Linux aarch64 binary
#   cli-darwin-x64/       — macOS Intel binary
#   cli-win32-x64/        — Windows x86_64 binary
#
# Binary source:
#   * Default: download release assets from GitHub Releases for tag v$VERSION.
#   * If RELEASE_ARTIFACTS_DIR is set, copy the assets from that local directory
#     instead (used in CI so we don't re-download what was just built).
#
# Usage:
#   scripts/build-cli-packages.sh [VERSION]
#   RELEASE_ARTIFACTS_DIR=/tmp/release scripts/build-cli-packages.sh 0.1.1
set -euo pipefail

VERSION=${1:-"0.1.1"}
REPO="Pogo-Bash/codonsplice"
BASE="https://github.com/$REPO/releases/download/v$VERSION"

# Map of platform-package → release asset
declare -A ASSETS=(
  ["cli-linux-x64"]="splice-linux-x86_64.tar.gz"
  ["cli-linux-arm64"]="splice-linux-aarch64.tar.gz"
  ["cli-darwin-x64"]="splice-macos-x86_64.tar.gz"
  ["cli-win32-x64"]="splice-windows-x86_64.exe"
)

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PKG_DIR="$ROOT/pkg"
TEMPLATES="$ROOT/scripts/cli-templates"

# Fetch an asset into $1 (output path). Uses the local artifacts dir when
# RELEASE_ARTIFACTS_DIR is set, otherwise curls from GitHub Releases.
fetch_asset() {
  local asset="$1" out="$2"
  if [[ -n "${RELEASE_ARTIFACTS_DIR:-}" ]]; then
    echo "  (local) $RELEASE_ARTIFACTS_DIR/$asset"
    cp "$RELEASE_ARTIFACTS_DIR/$asset" "$out"
  else
    echo "  (download) $BASE/$asset"
    curl -fsSL "$BASE/$asset" -o "$out"
  fi
}

# ── Main package: pkg/cli/ ──────────────────────────────────────────────────
echo "▶ assembling pkg/cli (main package)…"
mkdir -p "$PKG_DIR/cli/bin"
cp "$TEMPLATES/binary.js" "$PKG_DIR/cli/binary.js"
cp "$TEMPLATES/install.js" "$PKG_DIR/cli/install.js"
cp "$TEMPLATES/splice-wrapper.js" "$PKG_DIR/cli/bin/splice"
chmod +x "$PKG_DIR/cli/bin/splice"
cp "$TEMPLATES/splice.cmd" "$PKG_DIR/cli/bin/splice.cmd"

cat > "$PKG_DIR/cli/package.json" <<EOF
{
  "name": "@codonsplice/cli",
  "version": "$VERSION",
  "description": "CodonSplice genomic query engine — splice CLI",
  "license": "MIT",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/Pogo-Bash/codonsplice.git"
  },
  "homepage": "https://swapdoesbioandis-a.dev/splice",
  "keywords": ["genomics", "bioinformatics", "cli", "spliceql"],
  "os": ["linux", "darwin", "win32"],
  "cpu": ["x64", "arm64"],
  "scripts": { "postinstall": "node install.js" },
  "bin": { "splice": "bin/splice" },
  "files": ["install.js", "binary.js", "bin/"],
  "optionalDependencies": {
    "@codonsplice/cli-linux-x64": "$VERSION",
    "@codonsplice/cli-linux-arm64": "$VERSION",
    "@codonsplice/cli-darwin-x64": "$VERSION",
    "@codonsplice/cli-win32-x64": "$VERSION"
  }
}
EOF
echo "✓ cli ready"

# ── Platform packages: pkg/cli-<platform>/ ──────────────────────────────────
SKIPPED=()
for pkg in "${!ASSETS[@]}"; do
  asset="${ASSETS[$pkg]}"
  dir="$PKG_DIR/$pkg"

  # In local-artifacts mode, a binary that hasn't been built yet (e.g. the
  # macOS leg still queued in CI) is skipped rather than aborting the whole
  # run — its package can be assembled and published later from the same tag.
  if [[ -n "${RELEASE_ARTIFACTS_DIR:-}" && ! -f "$RELEASE_ARTIFACTS_DIR/$asset" ]]; then
    echo "⚠ skipping pkg/$pkg — $asset not present in $RELEASE_ARTIFACTS_DIR"
    SKIPPED+=("$pkg")
    continue
  fi

  mkdir -p "$dir/bin"
  echo "▶ assembling pkg/$pkg ($asset)…"

  if [[ "$asset" == *.tar.gz ]]; then
    tmp="$(mktemp)"
    fetch_asset "$asset" "$tmp"
    tar xz -C "$dir/bin" -f "$tmp" splice
    chmod +x "$dir/bin/splice"
    rm -f "$tmp"
  else
    # Windows .exe — store verbatim.
    fetch_asset "$asset" "$dir/bin/splice.exe"
  fi

  # Determine os/cpu from package name.
  case "$pkg" in
    *linux*)  os_val="linux" ;;
    *darwin*) os_val="darwin" ;;
    *win32*)  os_val="win32" ;;
  esac
  case "$pkg" in
    *arm64*) cpu_val="arm64" ;;
    *x64*)   cpu_val="x64" ;;
  esac

  cat > "$dir/package.json" <<EOF
{
  "name": "@codonsplice/$pkg",
  "version": "$VERSION",
  "description": "CodonSplice CLI — $os_val $cpu_val binary",
  "license": "MIT",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/Pogo-Bash/codonsplice.git"
  },
  "os": ["$os_val"],
  "cpu": ["$cpu_val"],
  "files": ["bin/"]
}
EOF

  echo "✓ $pkg ready"
done

echo ""
if [[ ${#SKIPPED[@]} -gt 0 ]]; then
  echo "⚠ skipped (binary not available yet): ${SKIPPED[*]}"
  echo "  assemble + publish these later from the same tag once built."
  echo ""
fi
echo "All CLI packages ready in pkg/cli*/"
echo "Publish with:"
echo "  for d in pkg/cli pkg/cli-linux-x64 pkg/cli-linux-arm64 pkg/cli-darwin-x64 pkg/cli-win32-x64; do"
echo "    (cd \$d && npm publish --access public)"
echo "  done"
