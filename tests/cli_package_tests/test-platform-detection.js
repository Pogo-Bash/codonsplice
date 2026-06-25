#!/usr/bin/env node
'use strict'
// Unit test for the @codonsplice/cli platform-detection logic in
// scripts/cli-templates/binary.js. Exercises every supported os/cpu pair and
// confirms an unsupported platform throws a helpful error.
//
// Run: node tests/cli_package_tests/test-platform-detection.js
const assert = require('assert')
const os = require('os')
const path = require('path')

const binaryPath = path.resolve(
  __dirname, '..', '..', 'scripts', 'cli-templates', 'binary.js'
)
const { getPlatformPackage } = require(binaryPath)

// binary.js reads os.platform()/os.arch() at call time, so we can stub them.
const realPlatform = os.platform
const realArch = os.arch
function withPlatform(platform, arch, fn) {
  os.platform = () => platform
  os.arch = () => arch
  try {
    return fn()
  } finally {
    os.platform = realPlatform
    os.arch = realArch
  }
}

const cases = [
  ['linux', 'x64', '@codonsplice/cli-linux-x64'],
  ['linux', 'arm64', '@codonsplice/cli-linux-arm64'],
  ['darwin', 'x64', '@codonsplice/cli-darwin-x64'],
  ['win32', 'x64', '@codonsplice/cli-win32-x64'],
]

let passed = 0
for (const [platform, arch, expected] of cases) {
  const got = withPlatform(platform, arch, getPlatformPackage)
  assert.strictEqual(
    got, expected,
    `expected ${platform}-${arch} → ${expected}, got ${got}`
  )
  console.log(`✓ ${platform}-${arch} → ${got}`)
  passed++
}

// Unsupported platforms must throw with a helpful, actionable message.
const unsupported = [
  ['darwin', 'arm64'], // Apple Silicon is not yet built
  ['freebsd', 'x64'],
  ['linux', 'ia32'],
]
for (const [platform, arch] of unsupported) {
  assert.throws(
    () => withPlatform(platform, arch, getPlatformPackage),
    (err) => {
      assert.ok(
        /unsupported platform/.test(err.message),
        `error should mention "unsupported platform": ${err.message}`
      )
      assert.ok(
        /cargo install splice-cli/.test(err.message),
        `error should suggest building from source: ${err.message}`
      )
      return true
    },
    `expected ${platform}-${arch} to throw`
  )
  console.log(`✓ ${platform}-${arch} → throws helpful error`)
  passed++
}

console.log(`\nAll ${passed} platform-detection checks passed.`)
