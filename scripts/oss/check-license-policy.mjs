#!/usr/bin/env node
// Dependency license policy gate for the Node side of the monorepo.
//
// The Cargo side is enforced by cargo-deny against `deny.toml`; this script is the
// equivalent for the pnpm graph, so a contributor adding an npm dependency gets the
// same "not explicitly allowed" failure as a contributor adding a crate.
//
// Run from the repository root, after `pnpm install --frozen-lockfile`:
//   node scripts/oss/check-license-policy.mjs
//
// pnpm reports the license string each package publishes, verbatim, so the allow
// list below is a list of those exact strings rather than a SPDX expression parser.
// A dependency that changes `MIT` to `MIT OR Apache-2.0` will trip this gate and
// needs its new string added here on purpose.
import assert from 'node:assert/strict'
import { execFileSync, execSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join, relative, resolve } from 'node:path'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
assert.equal(relative(root, process.cwd()), '', 'run this script from the repository root')

const PNPM_VERSION = '11.10.0'

// Permissive strings observed in the current graphs, plus their close relatives.
const ALLOWED = new Set([
  '0BSD',
  'Apache-2.0',
  'Apache-2.0 OR MIT',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'BlueOak-1.0.0',
  'ISC',
  'MIT',
  // Weak copyleft at file granularity (axe-core's accessibility engine, dev-only).
  'MPL-2.0',
  // Self-hosted variable fonts; the OFL text is reproduced in THIRD_PARTY_NOTICES.md.
  'OFL-1.1',
  // `argparse` publishes Python-2.0 metadata. It is a permissive PSF-style grant with no
  // source-disclosure or copyleft obligation; nothing here links against CPython.
  'Python-2.0',
  'Unlicense',
  '(MIT OR CC0-1.0)',
])

// Rejected even if someone adds them to ALLOWED by mistake: these carry source or
// copyleft obligations that do not belong in an Apache-2.0 product's dependency set.
const FORBIDDEN = [
  { token: /(^|[^A-Za-z0-9])(AGPL|GNU Affero)([^A-Za-z0-9]|$)/i, why: 'network copyleft' },
  { token: /(^|[^A-Za-z0-9])(SSPL|Server Side Public)([^A-Za-z0-9]|$)/i, why: 'server-side copyleft' },
  { token: /(^|[^A-Za-z0-9])GPL([^A-Za-z0-9]|$)/i, why: 'strong copyleft' },
  { token: /(^|[^A-Za-z0-9])LGPL([^A-Za-z0-9]|$)/i, why: 'linked-library copyleft' },
  { token: /BUSL|Business Source/i, why: 'source-available, not open source' },
  { token: /Elastic-2\.0|Confluent/i, why: 'source-available, not open source' },
  { token: /NC|Non-?Commercial/i, why: 'non-commercial' },
]

// Dev-only dependencies whose published metadata carries no license string at all.
// Production dependencies have no such escape: an unknown license fails the gate.
const DEV_UNKNOWN_EXCEPTIONS = new Map([
  [
    'abbrev@1.0.3',
    'dev-only transitive of js-beautify -> nopt@6; the package ships no license field, ' +
      'while the newer abbrev major already in this graph is dual ISC/MIT. Not present in ' +
      'any built artifact: it never enters the admin bundle or the desktop app.',
  ],
])

function licensesJson(args) {
  const argv = [`pnpm@${PNPM_VERSION}`, 'licenses', 'list', ...args, '--json']
  const options = { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 }
  let out
  if (process.platform === 'win32') {
    // corepack installs as a .cmd shim, which Node refuses to run without a shell. Hand it one
    // fully-quoted string instead of an argv array: every token below is a literal in this file,
    // and args-as-array-with-shell is the combination Node deprecates for exactly this reason.
    out = execSync(`corepack ${argv.map((token) => `"${token}"`).join(' ')}`, options)
  } else {
    out = execFileSync('corepack', argv, options)
  }
  // corepack can prefix a download banner on first use; the payload is one JSON object.
  return JSON.parse(out.slice(out.indexOf('{')))
}

function collect(payload) {
  const found = []
  for (const [license, packages] of Object.entries(payload)) {
    for (const pkg of packages) {
      for (const version of pkg.versions ?? ['']) found.push({ license, name: pkg.name, version })
    }
  }
  return found
}

const production = collect(licensesJson(['--prod']))
const full = collect(licensesJson([]))
const prodKeys = new Set(production.map((e) => `${e.name}@${e.version}`))
const development = full.filter((e) => !prodKeys.has(`${e.name}@${e.version}`))

const failures = []
for (const entry of [...production, ...development]) {
  const key = `${entry.name}@${entry.version}`
  const isProduction = prodKeys.has(key)
  const forbidden = FORBIDDEN.find((f) => f.token.test(entry.license))
  if (forbidden) {
    failures.push(`${key} [${entry.license}] is forbidden: ${forbidden.why}`)
    continue
  }
  if (ALLOWED.has(entry.license)) continue
  if (entry.license === 'Unknown' && !isProduction && DEV_UNKNOWN_EXCEPTIONS.has(key)) continue
  failures.push(
    `${key} [${entry.license}] is not allowed${isProduction ? ' in the production graph' : ''}. ` +
      'Add the exact string to ALLOWED in this script with a reason, or drop the dependency.',
  )
}

const summary = (rows) => {
  const byLicense = new Map()
  for (const r of rows) byLicense.set(r.license, (byLicense.get(r.license) ?? 0) + 1)
  return [...byLicense].sort((a, b) => b[1] - a[1]).map(([l, n]) => `${n}x ${l}`).join(', ')
}

console.log(`production dependencies: ${production.length} (${summary(production)})`)
console.log(`development-only dependencies: ${development.length} (${summary(development)})`)
for (const [key, why] of DEV_UNKNOWN_EXCEPTIONS) {
  if (development.some((e) => `${e.name}@${e.version}` === key)) console.log(`exception ${key}: ${why}`)
}

if (failures.length > 0) {
  console.error(`\nlicense policy FAILED with ${failures.length} finding(s):`)
  for (const failure of failures) console.error(`  - ${failure}`)
  process.exit(1)
}
console.log('license policy ok')
