#!/usr/bin/env node
// Self-test for the public-tree gate: proves the gate can still fail.
//
// A hygiene gate that cannot go red is documentation, not a control. Each case below runs
// the shipped scanner the way CI runs it, against a throwaway tree in the OS temp
// directory, and asserts both directions: a planted violation exits non-zero, a benign
// tree exits zero.
//
// The refusal of encoded pattern fields is asserted here rather than left to the scanner's
// own startup check, so that the guarantee this repository never carries its deny list in
// disguise is verified on every run - and so a later edit that re-adds decoding support
// fails CI instead of quietly matching nothing.
//
// Run: node scripts/oss/selftest-check-public-tree.mjs
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const scanner = join(here, 'check-public-tree.mjs')
const scannerSource = readFileSync(scanner, 'utf8')
const shippedDatabase = JSON.parse(readFileSync(join(here, 'patterns.json'), 'utf8'))

// The field names that would smuggle a signature past a reader. Listed as identifiers,
// not as JSON keys, so this file stays clean under the encoded-field search that
// docs/development/testing.md tells a reviewer to run over scripts/oss.
const encodedFields = ['b64', 'base64', 'encodedPattern', 'encodedSecret', 'hiddenPattern']

const scratch = mkdtempSync(join(tmpdir(), 'promptdock-gate-selftest-'))
const failures = []

function report(name, passed, detail) {
  if (passed) {
    console.log(`  ok   ${name}`)
  } else {
    console.error(`  FAIL ${name}${detail ? `: ${detail}` : ''}`)
    failures.push(name)
  }
}

function runScanner(script, tree) {
  const result = spawnSync(process.execPath, [script, tree], { encoding: 'utf8' })
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` }
}

function makeTree(name, text) {
  const root = join(scratch, name)
  mkdirSync(root, { recursive: true })
  writeFileSync(join(root, 'notes.txt'), text, 'utf8')
  return root
}

// A scanner copy fed a crafted database: the real one is never written to, because the
// gate has no flag for an alternate database and its own directory is where it looks.
function runScannerWithDatabase(name, database, tree) {
  const root = join(scratch, `config-${name}`)
  mkdirSync(root, { recursive: true })
  const script = join(root, 'check-public-tree.mjs')
  writeFileSync(script, scannerSource, 'utf8')
  writeFileSync(join(root, 'patterns.json'), JSON.stringify(database), 'utf8')
  return runScanner(script, tree)
}

function database(contentPatterns) {
  return { pathPatterns: [], contentPatterns, skipDirectories: [], binaryExtensions: [] }
}

const clean = makeTree('clean', 'Ordinary source text about nothing sensitive.\n')

// Assembled at runtime rather than written out: a literal in this file would be exactly
// the shape the detector under test reports, and the gate scans this script too. The
// documentation addresses below need no such treatment - the rule exempts them.
const personalPath = ['C:', 'Users', 'probe', 'scratch.txt'].join(String.fromCharCode(92))
const publicAddress = ['8', '8', '8', '8'].join('.')
const leak = makeTree('leak', `A staged note.\npath: ${personalPath}\n`)

const reserved = makeTree(
  'reserved',
  [
    '0.0.0.0',
    '127.0.0.1',
    '10.0.0.5',
    '172.16.0.1',
    '172.20.0.1',
    '100.64.0.1',
    '169.254.1.1',
    '192.168.1.10',
    '192.0.2.1',
    '198.51.100.1',
    '203.0.113.7',
    '255.255.255.255',
  ].join('\n') + '\n',
)
const routable = makeTree('routable', `resolver = ${publicAddress}\n`)
const sentenceEnd = makeTree('sentence-end', `Configure the resolver as ${publicAddress}.\n`)
const versionLike = makeTree('version', 'release 1.2.3.4.5 and v1.2.3.4 and 1.2.3\n')

console.log('Public-tree gate self-test')

for (const field of encodedFields) {
  // The valid regex stays in: an encoded field must be refused even when a usable regex
  // sits beside it, otherwise the check would only fire on an already-broken entry.
  const entry = { id: `probe-${field}`, flags: '', regex: 'nothing sensitive' }
  entry[field] = 'cHJvYmU='
  const result = runScannerWithDatabase(field, database([entry]), clean)
  report(
    `a pattern carrying "${field}" is refused before any file is read`,
    result.status !== 0 && result.output.includes(`probe-${field}`) && /forbidden/i.test(result.output),
    `exit ${result.status}: ${result.output.trim().slice(0, 160)}`,
  )
}

const shapeCases = [
  ['declares no regex', { id: 'probe-no-regex' }, /must declare "regex"/],
  ['declares an invalid regex', { id: 'probe-bad-regex', regex: '([' }, /invalid regular expression/],
  ['declares no id', { regex: 'nothing sensitive' }, /must declare a non-empty "id"/],
]
for (const [name, entry, expected] of shapeCases) {
  const result = runScannerWithDatabase(entry.id ?? 'idless', database([entry]), clean)
  report(
    `a pattern that ${name} stops the run`,
    result.status !== 0 && /failed to start/i.test(result.output) && expected.test(result.output),
    `exit ${result.status}: ${result.output.trim().slice(0, 160)}`,
  )
}

const caughtLeak = runScanner(scanner, leak)
report(
  'a planted personal absolute path is reported',
  caughtLeak.status !== 0 && caughtLeak.output.includes('personal-absolute-path'),
  `exit ${caughtLeak.status}: ${caughtLeak.output.trim().slice(0, 160)}`,
)

const acceptedClean = runScanner(scanner, clean)
report(
  'a benign tree is accepted',
  acceptedClean.status === 0,
  `exit ${acceptedClean.status}: ${acceptedClean.output.trim().slice(0, 160)}`,
)

const reservedResult = runScanner(scanner, reserved)
report(
  'loopback, private, link-local, shared and documentation addresses are not reported',
  reservedResult.status === 0 && !reservedResult.output.includes('routable-ipv4-literal'),
  `exit ${reservedResult.status}: ${reservedResult.output.trim().slice(0, 200)}`,
)

const routableResult = runScanner(scanner, routable)
report(
  'a routable IPv4 literal is reported',
  routableResult.status !== 0 && routableResult.output.includes('routable-ipv4-literal'),
  `exit ${routableResult.status}: ${routableResult.output.trim().slice(0, 200)}`,
)

const versionResult = runScanner(scanner, versionLike)
report(
  'a dotted version string is not mistaken for an address',
  versionResult.status === 0 && !versionResult.output.includes('routable-ipv4-literal'),
  `exit ${versionResult.status}: ${versionResult.output.trim().slice(0, 200)}`,
)

const sentenceEndResult = runScanner(scanner, sentenceEnd)
report(
  'an address ending a sentence is still reported',
  sentenceEndResult.status !== 0 && sentenceEndResult.output.includes('routable-ipv4-literal'),
  `exit ${sentenceEndResult.status}: ${sentenceEndResult.output.trim().slice(0, 200)}`,
)

const allEntries = [...(shippedDatabase.pathPatterns ?? []), ...(shippedDatabase.contentPatterns ?? [])]
report(
  `the shipped database (${allEntries.length} patterns) carries no encoded field`,
  allEntries.every((entry) => encodedFields.every((field) => !(field in entry))),
)
report(
  'every shipped pattern declares a non-empty regex',
  allEntries.every((entry) => typeof entry.regex === 'string' && entry.regex.length > 0),
)

rmSync(scratch, { recursive: true, force: true })

if (failures.length > 0) {
  console.error(`\nPublic-tree gate self-test FAILED: ${failures.length} case(s)`)
  process.exit(1)
}
console.log(
  '\nPublic-tree gate self-test passed: the gate refuses encoded patterns, stops on ones it ' +
    'cannot interpret, still fires on planted values and still passes a clean tree.',
)
