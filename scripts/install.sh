#!/usr/bin/env sh
# CodonSplice installer — auto-detects OS/arch and downloads the matching
# prebuilt `splice` binary from the latest GitHub release.
#
#   curl -fsSL https://github.com/Pogo-Bash/codonsplice/releases/latest/download/install.sh | sh
#
# Override the install dir with INSTALL_DIR=~/bin sh install.sh
set -eu

REPO="Pogo-Bash/codonsplice"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)  os_tag="linux" ;;
  Darwin) os_tag="macos" ;;
  *) echo "unsupported OS: $os (try cargo install codonsplice)"; exit 1 ;;
esac

case "$arch" in
  x86_64|amd64)  arch_tag="x86_64" ;;
  arm64|aarch64) arch_tag="aarch64" ;;
  *) echo "unsupported arch: $arch"; exit 1 ;;
esac

asset="splice-${os_tag}-${arch_tag}"
url="https://github.com/${REPO}/releases/latest/download/${asset}"

echo "▶ downloading ${asset}…"
tmp="$(mktemp)"
curl -fsSL "$url" -o "$tmp"
chmod +x "$tmp"

if [ -w "$INSTALL_DIR" ]; then
  mv "$tmp" "$INSTALL_DIR/splice"
else
  echo "▶ ${INSTALL_DIR} needs sudo…"
  sudo mv "$tmp" "$INSTALL_DIR/splice"
fi

echo "✓ installed splice to ${INSTALL_DIR}/splice"
"${INSTALL_DIR}/splice" --version || true
