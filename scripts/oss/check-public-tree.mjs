#!/usr/bin/env node
// Public-tree gate: refuses forbidden repository paths, personal-data shapes and live
// credential literals anywhere in the shipped tree. Exceptions are precise per-pattern
// path allowlists declared in patterns.json; there is no global ignore and no
// self-exemption.
//
// This public scanner contains generic repository hygiene and credential-shape rules
// only. Private environment-specific deny lists must be kept outside the repository:
// an identifier for a real host, address, account or working directory is disclosed by
// storing it here in any form, encoded included. The gate therefore accepts `regex`
// and nothing else, so a pattern database that carries its signatures in disguise
// fails the run instead of quietly matching nothing.
// Run: node scripts/oss/check-public-tree.mjs [path/to/tree]
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const patterns = JSON.parse(readFileSync(join(here, 'patterns.json'), 'utf8'))
const scanRoot = resolve(process.argv[2] ?? join(here, '..', '..'))

const skipDirectories = new Set(patterns.skipDirectories ?? [])
const binaryExtensions = new Set((patterns.binaryExtensions ?? []).map((e) => e.toLowerCase()))
const maxFileBytes = patterns.maxFileBytes ?? 4 * 1024 * 1024

// Field names that mean "this entry hides its signature behind an encoding". Base64 is
// a reversible encoding, not a redaction, so none of them are accepted anywhere in the
// shipped database - not under this name, not under an alias invented later.
const encodedPatternFields = ['b64', 'base64', 'encodedPattern', 'encodedSecret', 'hiddenPattern']

function source(entry) {
  const id = typeof entry.id === 'string' && entry.id !== '' ? entry.id : '<missing id>'
  for (const field of encodedPatternFields) {
    if (field in entry) {
      throw new Error(
        `Pattern ${id} uses forbidden encoded pattern field "${field}". The public pattern ` +
          'database holds generic regexes only; keep environment-specific identifiers out of ' +
          'the repository instead of encoding them.',
      )
    }
  }
  if (typeof entry.id !== 'string' || entry.id === '') {
    throw new Error('Every pattern must declare a non-empty "id" so a finding is attributable')
  }
  if (typeof entry.regex !== 'string') {
    throw new Error(`Pattern ${entry.id} must declare "regex"`)
  }
  return entry.regex
}

function compile(entry) {
  const sourceText = source(entry)
  let re
  try {
    re = new RegExp(sourceText, entry.flags ?? '')
  } catch (error) {
    throw new Error(`Pattern ${entry.id} declares an invalid regular expression: ${error.message}`)
  }
  return { ...entry, re, allow: (entry.allow ?? []).map(toPosix) }
}

// Validated before any file is read: a database the scanner cannot interpret must stop
// the run, not narrow it.
let contentPatterns
let pathPatterns
try {
  contentPatterns = (patterns.contentPatterns ?? []).map(compile)
  pathPatterns = (patterns.pathPatterns ?? []).map(compile)
} catch (error) {
  console.error(`Public-tree gate failed to start: ${error.message}`)
  process.exit(1)
}

function toPosix(value) {
  return value.split(sep).join('/')
}

function isAllowed(relPath, allowList) {
  return allowList.some((entry) => {
    if (entry.endsWith('/')) return relPath.startsWith(entry)
    if (entry.endsWith('*')) return relPath.startsWith(entry.slice(0, -1))
    return relPath === entry
  })
}

function looksBinary(buffer) {
  const probe = buffer.subarray(0, Math.min(buffer.length, 8192))
  return probe.includes(0)
}

function* walk(directory) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const absolute = join(directory, entry.name)
    if (entry.isDirectory()) {
      if (skipDirectories.has(entry.name)) continue
      yield* walk(absolute)
    } else if (entry.isFile()) {
      yield absolute
    }
  }
}

function lineHits(text, re) {
  const lines = text.split(/\r?\n/)
  const hits = []
  for (let index = 0; index < lines.length; index += 1) {
    re.lastIndex = 0
    if (re.test(lines[index])) hits.push(index + 1)
    if (hits.length >= 5) break
  }
  return hits
}

const violations = []
let filesScanned = 0
let pathChecks = 0

for (const absolute of walk(scanRoot)) {
  const relPath = toPosix(relative(scanRoot, absolute))
  pathChecks += 1
  // Path rules are decided by the name, not the bytes, so they run before every content skip.
  // A 40 MiB production database or credential bundle is the exact thing they exist to catch,
  // and it is also the exact thing an extension/size skip would never open.
  for (const pattern of pathPatterns) {
    if (!isAllowed(relPath, pattern.allow) && pattern.re.test(relPath)) {
      violations.push({ kind: 'path', id: pattern.id, file: relPath, line: 0, description: pattern.description })
    }
  }
  const extIndex = relPath.lastIndexOf('.')
  if (extIndex >= 0 && binaryExtensions.has(relPath.slice(extIndex).toLowerCase())) continue
  if (statSync(absolute).size > maxFileBytes) continue

  const buffer = readFileSync(absolute)
  if (looksBinary(buffer)) continue

  const text = buffer.toString('utf8')
  filesScanned += 1
  for (const pattern of contentPatterns) {
    if (isAllowed(relPath, pattern.allow)) continue
    const hits = lineHits(text, pattern.re)
    for (const line of hits) {
      violations.push({ kind: 'content', id: pattern.id, file: relPath, line, description: pattern.description })
    }
  }
}

if (violations.length > 0) {
  console.error(`Public-tree gate FAILED: ${violations.length} violation(s) under ${toPosix(relative(process.cwd(), scanRoot)) || scanRoot}`)
  for (const violation of violations) {
    const where = violation.kind === 'path' ? violation.file : `${violation.file}:${violation.line}`
    console.error(`  [${violation.kind}] ${violation.id} — ${where} — ${violation.description}`)
  }
  process.exit(1)
}

console.log(
  `Public-tree gate passed: ${pathChecks} file name(s) checked against ${pathPatterns.length} path ` +
    `pattern(s), of which ${filesScanned} text file(s) were scanned against ${contentPatterns.length} content pattern(s).`,
)
