// Validates the shared server-http v1 contract fixtures that live at the monorepo
// root (`contracts/server-http/v1/`). Desktop and server consume the same tree;
// no vendored snapshot, no source-repository pin, no per-release commit hash.
//
// Drift is still detected: every fixture file must match its declared SHA-256 in
// `manifest.json`, and the directory inventory must exactly equal the manifest.

import { createHash } from 'node:crypto'
import { readFile, readdir, lstat } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const CONTRACT = 'promptdock-relay-api-v1' as const
const MANIFEST_VERSION = 1
const FIXTURE_SCHEMA_VERSION = 1
export const EXPECTED_FIXTURE_COUNT = 16

const SHA256_PATTERN = /^[0-9a-f]{64}$/
const FIXTURE_PATH_PATTERN = /^[a-z0-9][a-z0-9.-]*-v1\.json$/

// apps/desktop/scripts/relay-contract-lib.ts  ->  repo root = ../../
// repo root /contracts/server-http/v1         ->  canonical fixtures
const desktopRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
export const repositoryRoot = resolve(desktopRoot, '..', '..')
export const relayApiV1ContractDirectory = resolve(repositoryRoot, 'contracts', 'server-http', 'v1')

type JsonObject = Record<string, unknown>

export type RelayContractManifestEntry = {
  path: string
  sha256: string
  mediaType: 'application/json'
  schemaVersion: 1
}

export type RelayContractManifest = {
  manifestVersion: 1
  contract: typeof CONTRACT
  fixtures: RelayContractManifestEntry[]
}

export type ValidatedRelayContract = {
  directory: string
  manifest: RelayContractManifest
  manifestSha256: string
}

function fail(message: string): never {
  throw new Error(message)
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function requireExactKeys(value: JsonObject, expected: readonly string[], label: string): void {
  const actual = Object.keys(value).sort()
  const wanted = [...expected].sort()
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    fail(`${label} must contain exactly: ${wanted.join(', ')}`)
  }
}

export function parseStrictJson(raw: Buffer, label: string): unknown {
  let text: string
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(raw)
  } catch {
    fail(`${label} is not valid UTF-8`)
  }
  try {
    return JSON.parse(text) as unknown
  } catch {
    fail(`${label} is not valid JSON`)
  }
}

export function sha256(raw: Buffer | string): string {
  return createHash('sha256').update(raw).digest('hex')
}

export function parseManifest(value: unknown): RelayContractManifest {
  if (!isObject(value)) fail('manifest.json must be an object')
  requireExactKeys(value, ['manifestVersion', 'contract', 'fixtures'], 'manifest.json')
  if (value.manifestVersion !== MANIFEST_VERSION) {
    fail(`manifest.json manifestVersion must be ${MANIFEST_VERSION}`)
  }
  if (value.contract !== CONTRACT) fail(`manifest.json contract must be ${CONTRACT}`)
  if (!Array.isArray(value.fixtures) || value.fixtures.length !== EXPECTED_FIXTURE_COUNT) {
    fail(`manifest.json must contain exactly ${EXPECTED_FIXTURE_COUNT} fixtures`)
  }

  const fixtures = value.fixtures.map((candidate, index) => {
    if (!isObject(candidate)) fail(`manifest fixture ${index} must be an object`)
    requireExactKeys(
      candidate,
      ['path', 'sha256', 'mediaType', 'schemaVersion'],
      `manifest fixture ${index}`,
    )
    if (
      typeof candidate.path !== 'string' ||
      candidate.path.length === 0 ||
      candidate.path !== candidate.path.trim() ||
      !FIXTURE_PATH_PATTERN.test(candidate.path)
    ) {
      fail(`manifest fixture ${index} has an unsafe path`)
    }
    if (typeof candidate.sha256 !== 'string' || !SHA256_PATTERN.test(candidate.sha256)) {
      fail(`manifest fixture ${candidate.path} has an invalid SHA-256`)
    }
    if (candidate.mediaType !== 'application/json') {
      fail(`manifest fixture ${candidate.path} must use application/json`)
    }
    if (candidate.schemaVersion !== FIXTURE_SCHEMA_VERSION) {
      fail(`manifest fixture ${candidate.path} must use schemaVersion ${FIXTURE_SCHEMA_VERSION}`)
    }
    return candidate as RelayContractManifestEntry
  })

  const paths = fixtures.map((fixture) => fixture.path)
  const sortedPaths = [...paths].sort()
  if (
    new Set(paths).size !== paths.length ||
    paths.some((path, index) => path !== sortedPaths[index])
  ) {
    fail('manifest fixture paths must be unique and sorted')
  }
  return { manifestVersion: 1, contract: CONTRACT, fixtures }
}

async function requireRegularDirectory(directory: string, label: string): Promise<void> {
  const metadata = await lstat(directory).catch(() => fail(`${label} is missing or unreadable`))
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    fail(`${label} must be a regular directory, not a file or symbolic link`)
  }
}

async function validateInventory(directory: string, expected: readonly string[]): Promise<void> {
  const entries = await readdir(directory, { withFileTypes: true }).catch(() =>
    fail(`contract directory is unreadable: ${directory}`),
  )
  if (entries.some((entry) => !entry.isFile())) {
    fail('contract inventory may contain regular files only')
  }
  const actual = entries.map((entry) => entry.name).sort()
  const wanted = [...expected].sort()
  if (actual.length !== wanted.length || actual.some((name, index) => name !== wanted[index])) {
    fail(`contract inventory mismatch; expected [${wanted.join(', ')}], got [${actual.join(', ')}]`)
  }
}

async function validateFixtureFiles(
  directory: string,
  manifest: RelayContractManifest,
): Promise<void> {
  for (const fixture of manifest.fixtures) {
    const raw = await readFile(join(directory, fixture.path)).catch(() =>
      fail(`fixture is missing or unreadable: ${fixture.path}`),
    )
    const actualHash = sha256(raw)
    if (actualHash !== fixture.sha256) {
      fail(
        `fixture SHA-256 mismatch for ${fixture.path}: expected ${fixture.sha256}, got ${actualHash}`,
      )
    }
    parseStrictJson(raw, fixture.path)
  }
}

export async function validateRelayApiV1Contract(
  directory = relayApiV1ContractDirectory,
): Promise<ValidatedRelayContract> {
  const resolved = resolve(directory)
  await requireRegularDirectory(resolved, 'Relay HTTP v1 contract directory')
  const manifestRaw = await readFile(join(resolved, 'manifest.json')).catch(() =>
    fail('manifest.json is missing or unreadable'),
  )
  const manifest = parseManifest(parseStrictJson(manifestRaw, 'manifest.json'))
  await validateInventory(resolved, ['manifest.json', ...manifest.fixtures.map((f) => f.path)])
  await validateFixtureFiles(resolved, manifest)
  return { directory: resolved, manifest, manifestSha256: sha256(manifestRaw) }
}
