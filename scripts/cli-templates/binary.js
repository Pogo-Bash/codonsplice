'use strict'
// Resolves the platform-specific `splice` binary shipped by the matching
// @codonsplice/cli-<platform> optional dependency. Modeled on esbuild's
// install strategy: the main package is platform-agnostic and only the
// sub-package for the host's os/cpu is actually installed by npm.
const os = require('os')

function getPlatformPackage() {
  const platform = os.platform()
  const arch = os.arch()

  const map = {
    'linux-x64': '@codonsplice/cli-linux-x64',
    'linux-arm64': '@codonsplice/cli-linux-arm64',
    'darwin-x64': '@codonsplice/cli-darwin-x64',
    'darwin-arm64': '@codonsplice/cli-darwin-arm64',
    'win32-x64': '@codonsplice/cli-win32-x64',
  }

  const key = `${platform}-${arch}`
  const pkg = map[key]

  if (!pkg) {
    throw new Error(
      `@codonsplice/cli: unsupported platform ${platform}-${arch}\n` +
      `Supported: ${Object.keys(map).join(', ')}\n` +
      `Install from source: cargo install splice-cli`
    )
  }
  return pkg
}

function getBinaryPath() {
  const pkg = getPlatformPackage()
  try {
    return require.resolve(`${pkg}/bin/splice${
      os.platform() === 'win32' ? '.exe' : ''
    }`)
  } catch (e) {
    throw new Error(
      `@codonsplice/cli: could not find binary for ${pkg}\n` +
      `Try: npm install ${pkg}`
    )
  }
}

module.exports = { getBinaryPath, getPlatformPackage }
