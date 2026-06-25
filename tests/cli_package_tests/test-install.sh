#!/usr/bin/env bash
# End-to-end smoke test for @codonsplice/cli: install it into a throwaway
# directory, run `splice --version`, and confirm the binary actually ran.
#
# By default this installs the published package from the npm registry. To test
# locally built tarballs first, pack the packages and point at them:
#
#   for d in pkg/cli-linux-x64 pkg/cli; do (cd "$d" && npm pack --pack-destination /tmp/cli-tars); done
#   CLI_TARBALL_DIR=/tmp/cli-tars tests/cli_package_tests/test-install.sh
#
# Env:
#   CLI_VERSION       version/tag to install from npm (default: latest)
#   CLI_TARBALL_DIR   if set, install the *.tgz tarballs in this dir instead
set -euo pipefail

VERSION="${CLI_VERSION:-latest}"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

echo "▶ test dir: $WORKDIR"
cd "$WORKDIR"
npm init -y >/dev/null 2>&1

if [[ -n "${CLI_TARBALL_DIR:-}" ]]; then
  echo "▶ installing local tarballs from $CLI_TARBALL_DIR"
  # Install the platform package(s) and the main package together so the
  # optionalDependency is satisfied from local disk.
  npm install --no-audit --no-fund "$CLI_TARBALL_DIR"/*.tgz
else
  echo "▶ installing @codonsplice/cli@$VERSION from npm"
  npm install --no-audit --no-fund "@codonsplice/cli@$VERSION"
fi

# Resolve the installed bin and run it.
SPLICE="$WORKDIR/node_modules/.bin/splice"
if [[ ! -x "$SPLICE" ]]; then
  echo "✗ splice bin not found at $SPLICE"
  ls -la "$WORKDIR/node_modules/.bin" || true
  exit 1
fi

echo "▶ splice --version"
out="$("$SPLICE" --version 2>&1)"
echo "  $out"

if ! grep -qi 'splice' <<<"$out"; then
  echo "✗ expected output to contain 'splice', got: $out"
  exit 1
fi

echo "✓ @codonsplice/cli installed and ran successfully"
