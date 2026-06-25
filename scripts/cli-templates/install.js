'use strict'
// Runs at npm install time (postinstall). Verifies the platform binary is
// available. If not, prints a helpful warning but does NOT fail the install —
// the optional platform sub-package may be installed separately, and failing
// here would abort installs on unsupported platforms entirely.
const { getBinaryPath } = require('./binary')

try {
  const p = getBinaryPath()
  console.log(`@codonsplice/cli: splice binary at ${p}`)
} catch (e) {
  console.warn(`@codonsplice/cli: ${e.message}`)
  console.warn(
    'The splice binary will not be available until ' +
    'the platform package is installed.'
  )
}
