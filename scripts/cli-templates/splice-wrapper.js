#!/usr/bin/env node
'use strict'
// Thin launcher installed as the `splice` bin. Resolves the real platform
// binary via ../binary.js (which lives at the package root) and forwards all
// args + stdio to it.
const { spawnSync } = require('child_process')
const result = spawnSync(
  require('../binary.js').getBinaryPath(),
  process.argv.slice(2),
  { stdio: 'inherit' }
)
process.exit(result.status ?? 1)
