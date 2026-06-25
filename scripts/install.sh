#!/usr/bin/env sh
# CodonSplice installer — a guided TUI installer in pure POSIX sh.
#
#   curl -fsSL https://github.com/Pogo-Bash/codonsplice/releases/latest/download/install.sh | sh
#
# Needs: curl, tar. Uses tput for colour/cursor control, with a plain-text
# fallback when tput is unavailable. Override the install dir with
# INSTALL_DIR=~/bin sh install.sh
set -eu

REPO="Pogo-Bash/codonsplice"
DOCS_URL="https://swapdoesbioandis-a.dev/splice"
VERSION_LABEL="0.1.0"

# ── colours (tput with fallback) ─────────────────────────────────────────────
if command -v tput >/dev/null 2>&1 && tput colors >/dev/null 2>&1; then
  GREEN=$(tput setaf 10 2>/dev/null || tput setaf 2)
  BLUE=$(tput setaf 12 2>/dev/null || tput setaf 4)
  YELLOW=$(tput setaf 11 2>/dev/null || tput setaf 3)
  RED=$(tput setaf 9 2>/dev/null || tput setaf 1)
  BOLD=$(tput bold)
  RESET=$(tput sgr0)
  HAS_TPUT=1
else
  GREEN="" BLUE="" YELLOW="" RED="" BOLD="" RESET=""
  HAS_TPUT=0
fi

cls() { if [ "$HAS_TPUT" = 1 ]; then tput clear; else printf '\n\n'; fi; }

# Cleanup + Ctrl-C handling.
TMP_TGZ=""
TMP_DIR=""
cleanup() {
  if [ -n "$TMP_TGZ" ]; then rm -f "$TMP_TGZ" 2>/dev/null || true; fi
  if [ -n "$TMP_DIR" ]; then rm -rf "$TMP_DIR" 2>/dev/null || true; fi
}
trap 'echo ""; echo "Install cancelled."; cleanup; exit 130' INT
trap 'cleanup' EXIT

# Literal UTF-8 symbols (POSIX printf does not interpret \xNN hex escapes).
ok()   { printf '%s✓%s  %s\n' "$GREEN" "$RESET" "$1"; }
arrow(){ printf '%s→%s  %s\n' "$YELLOW" "$RESET" "$1"; }
bad()  { printf '%s✗%s  %s\n' "$RED" "$RESET" "$1"; }
rule() { printf '%s\n' "-----------------------------------------------"; }
header() {
  printf '%s%sCodonSplice Installer%s\n' "$BOLD" "$BLUE" "$RESET"
  rule
  printf '\n'
}

# Center a single line of given display width using the terminal width.
center() {
  _text=$1; _width=$2
  if [ "$HAS_TPUT" = 1 ]; then
    _cols=$(tput cols 2>/dev/null || echo 80)
    _pad=$(( (_cols - _width) / 2 ))
    [ "$_pad" -lt 0 ] && _pad=0
    printf '%*s%s\n' "$_pad" "" "$_text"
  else
    printf '%s\n' "$_text"
  fi
}

# Read a single line from the controlling terminal (works under `curl | sh`).
read_line() {
  if [ -r /dev/tty ]; then read -r REPLY </dev/tty || REPLY=""; else REPLY=""; fi
}

die_download() {
  bad "Download failed. Check your internet connection."
  printf '   If the problem persists, install manually:\n'
  printf '   https://github.com/%s/releases/latest\n' "$REPO"
  exit 1
}

# ── ASCII art ────────────────────────────────────────────────────────────────
codon_art() {
  printf '%s' "$GREEN"
  center '██████╗ ██████╗ ██████╗  ██████╗ ███╗  ██╗' 44
  center '██╔════╝██╔═══██╗██╔══██╗██╔═══██╗████╗ ██║' 44
  center '██║     ██║   ██║██║  ██║██║   ██║██╔██╗██║' 44
  center '██║     ██║   ██║██║  ██║██║   ██║██║╚████║' 44
  center '╚██████╗╚██████╔╝██████╔╝╚██████╔╝██║ ╚███║' 44
  center ' ╚═════╝ ╚═════╝ ╚═════╝  ╚═════╝ ╚═╝  ╚══╝' 44
  printf '%s' "$RESET"
}
splice_art() {
  printf '%s' "$BLUE"
  center '███████╗██████╗ ██╗      ██╗ ██████╗███████╗' 44
  center '██╔════╝██╔══██╗██║      ██║██╔════╝██╔════╝' 44
  center '███████╗██████╔╝██║      ██║██║     █████╗' 44
  center '╚════██║██╔═══╝ ██║      ██║██║     ██╔══╝' 44
  center '███████║██║     ███████╗ ██║╚██████╗███████╗' 44
  center '╚══════╝╚═╝     ╚══════╝ ╚═╝ ╚═════╝╚══════╝' 44
  printf '%s' "$RESET"
}

# ── platform detection ───────────────────────────────────────────────────────
detect_platform() {
  OS_NAME=$(uname -s)
  ARCH=$(uname -m)
  case "$OS_NAME" in
    Linux)  OS_TAG="linux" ;;
    Darwin) OS_TAG="macos" ;;
    *) bad "Unsupported OS: $OS_NAME (try: cargo install codonsplice)"; exit 1 ;;
  esac
  case "$ARCH" in
    x86_64|amd64)  ARCH_TAG="x86_64" ;;
    arm64|aarch64) ARCH_TAG="aarch64" ;;
    *) bad "Unsupported architecture: $ARCH"; exit 1 ;;
  esac
  TARGET="splice-${OS_TAG}-${ARCH_TAG}"
  INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
}

# ── SCREEN 1: welcome ────────────────────────────────────────────────────────
screen_welcome() {
  cls
  printf '\n'
  codon_art
  printf '\n'
  splice_art
  printf '\n'
  center "SpliceQL Genomic Query Engine" 29
  center "Installer v${VERSION_LABEL}" 18
  printf '\n'
  center "Press Enter to continue or Ctrl+C to exit" 41
  read_line
}

# ── SCREEN 2: environment detection ──────────────────────────────────────────
screen_detect() {
  cls; header
  printf 'Detecting environment...\n\n'
  ok "OS:           $OS_NAME"
  ok "Architecture: $ARCH"
  if command -v curl >/dev/null 2>&1; then ok "curl:         available"; else
    bad "curl:         not found"
    printf 'Cannot continue. Please install curl and re-run.\n'; exit 1; fi
  if command -v tar >/dev/null 2>&1; then ok "tar:          available"; else
    bad "tar:          not found"
    printf 'Cannot continue. Please install tar and re-run.\n'; exit 1; fi
  ok "Install dir:  $INSTALL_DIR"
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ok   "PATH:         $INSTALL_DIR already in PATH" ;;
    *)                  arrow "PATH:         will be added to your shell config" ;;
  esac
  printf '\n'
  printf 'Target binary:  %s\n' "$TARGET"
  printf 'Install path:   %s/splice\n\n' "$INSTALL_DIR"
  printf 'Continue? [Y/n] '
  read_line
  case "$REPLY" in
    n|N) printf 'Cancelled.\n'; exit 0 ;;
    *) : ;;
  esac
}

# ── SCREEN 3: install method ─────────────────────────────────────────────────
METHOD=binary
screen_method() {
  cls; header
  printf 'How would you like to install CodonSplice?\n\n'
  printf '  [1]  Download prebuilt binary  (recommended, fastest)\n'
  printf '  [2]  cargo install             (from source, slower)\n'
  printf '  [3]  Cancel\n\n'
  printf 'Enter choice [1]: '
  read_line
  case "$REPLY" in
    2) METHOD=cargo
       if ! command -v cargo >/dev/null 2>&1; then
         bad "cargo not found. Install Rust first:"
         printf "   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh\n"
         printf 'Then re-run this installer.\n'; exit 1
       fi ;;
    3) printf 'Cancelled.\n'; exit 0 ;;
    *) METHOD=binary ;;
  esac
}

# ── SCREEN 4: download + install ─────────────────────────────────────────────
fetch_version() {
  api="https://api.github.com/repos/$REPO/releases/latest"
  VERSION=$(curl -fsSL "$api" 2>/dev/null \
    | grep '"tag_name"' \
    | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/' \
    | head -n 1) || VERSION=""
  if [ -z "$VERSION" ]; then
    bad "Could not determine latest version."
    printf '   Check: https://github.com/%s/releases\n' "$REPO"
    exit 1
  fi
}

screen_install() {
  cls; header
  printf 'Installing CodonSplice...\n\n'

  if [ "$METHOD" = cargo ]; then
    printf 'Step 1/2  Running cargo install codonsplice...\n\n'
    cargo install codonsplice || { bad "cargo install failed."; exit 1; }
    printf '\nStep 2/2  Verifying...\n'
    return
  fi

  printf 'Step 1/4  Fetching latest release version...\n'
  fetch_version
  ok "Latest version: $VERSION"
  printf '\n'

  url="https://github.com/$REPO/releases/download/$VERSION/${TARGET}.tar.gz"
  TMP_TGZ="/tmp/splice_install_$$.tar.gz"
  TMP_DIR="/tmp/splice_$$"

  printf 'Step 2/4  Downloading %s...\n' "$TARGET"
  curl -fSL --progress-bar "$url" -o "$TMP_TGZ" || die_download
  size=$(wc -c < "$TMP_TGZ" 2>/dev/null || echo 0)
  ok "Downloaded ($(( size / 1024 )) KB)"
  printf '\n'

  printf 'Step 3/4  Installing to %s/...\n' "$INSTALL_DIR"
  mkdir -p "$INSTALL_DIR" "$TMP_DIR" || {
    bad "Install failed. You may need write permission to: $INSTALL_DIR"
    printf '   Try: mkdir -p %s\n' "$INSTALL_DIR"; exit 1; }
  tar -xzf "$TMP_TGZ" -C "$TMP_DIR" || { bad "Extraction failed."; exit 1; }
  # The archive contains the `splice` binary (possibly under a subdir).
  bin=$(find "$TMP_DIR" -name splice -type f 2>/dev/null | head -n 1)
  [ -z "$bin" ] && { bad "Could not find splice in the archive."; exit 1; }
  mv "$bin" "$INSTALL_DIR/splice" || { bad "Install failed (mv)."; exit 1; }
  chmod +x "$INSTALL_DIR/splice"
  rm -f "$TMP_TGZ"; rm -rf "$TMP_DIR"; TMP_TGZ=""; TMP_DIR=""
  ok "Installed to $INSTALL_DIR/splice"
  printf '\n'

  printf 'Step 4/4  Configuring PATH...\n'
  configure_path
}

configure_path() {
  case ":$PATH:" in
    *":$INSTALL_DIR:"*)
      ok "$INSTALL_DIR already in PATH — no changes needed"
      return ;;
  esac
  # Pick a shell config file.
  rc=""
  case "${SHELL:-}" in
    *zsh*) rc="$HOME/.zshrc" ;;
  esac
  if [ -z "$rc" ]; then
    if [ -f "$HOME/.bashrc" ]; then rc="$HOME/.bashrc"
    elif [ -f "$HOME/.bash_profile" ]; then rc="$HOME/.bash_profile"
    else rc="$HOME/.profile"; fi
  fi
  {
    printf '\n# CodonSplice\n'
    # The literal $PATH must be written verbatim into the rc file.
    # shellcheck disable=SC2016
    printf 'export PATH="%s:$PATH"\n' "$INSTALL_DIR"
  } >> "$rc"
  arrow "Added to $rc"
  PATH_UPDATED="$rc"
}

# ── SCREEN 5: success ────────────────────────────────────────────────────────
screen_success() {
  cls
  printf '\n'
  codon_art
  printf '\n'
  ver="${VERSION:-v$VERSION_LABEL}"
  ok "CodonSplice $ver installed successfully!"
  rule
  printf '\n'
  printf 'splice is installed at: %s/splice\n\n' "$INSTALL_DIR"
  if [ -n "${PATH_UPDATED:-}" ]; then
    printf 'PATH was updated. Restart your terminal or run:\n'
    printf '  source %s\n\n' "$PATH_UPDATED"
  fi
  printf 'Get started:\n\n'
  printf '  splice                  open the interactive TUI\n'
  printf '  splice --help           show all commands\n'
  printf "  splice check 'FROM bam \"sample.bam\" CALL variants'\n\n"
  printf 'Documentation:\n  %s\n\n' "$DOCS_URL"
  rule
  printf '\n'
  printf 'What would you like to do now?\n\n'
  printf '  [1]  Open the CodonSplice TUI\n'
  printf '  [2]  View documentation\n'
  printf '  [3]  Exit\n\n'
  printf 'Enter choice [3]: '
  read_line
  case "$REPLY" in
    1) if command -v splice >/dev/null 2>&1; then exec splice
       else "$INSTALL_DIR/splice" || true; fi ;;
    2) open_url "$DOCS_URL" ;;
    *) exit 0 ;;
  esac
}

open_url() {
  if [ "$OS_TAG" = macos ]; then open "$1" 2>/dev/null || true
  else xdg-open "$1" 2>/dev/null || true; fi
}

# ── main ─────────────────────────────────────────────────────────────────────
main() {
  detect_platform
  screen_welcome
  screen_detect
  screen_method
  screen_install
  screen_success
}

main "$@"
