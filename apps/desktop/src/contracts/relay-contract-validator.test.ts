import { cp, mkdtemp, rm, unlink, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { afterEach, describe, expect, it } from 'vitest'

import {
  CONTRACT,
  EXPECTED_FIXTURE_COUNT,
  parseManifest,
  parseStrictJson,
  relayApiV1ContractDirectory,
  validateRelayApiV1Contract,
} from '../../scripts/relay-contract-lib.ts'

const temporaryRoots: string[] = []

async function temporaryRoot(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), 'promptdock-contract-'))
  temporaryRoots.push(root)
  return root
}

async function copyContract(): Promise<string> {
  const root = await temporaryRoot()
  const destination = join(root, 'contract')
  await cp(relayApiV1ContractDirectory, destination, { recursive: true })
  return destination
}

afterEach(async () => {
  await Promise.all(
    temporaryRoots.splice(0).map((root) => rm(root, { recursive: true, force: true })),
  )
})

describe('Relay HTTP v1 contract validator', () => {
  it('accepts the canonical monorepo contract directory', async () => {
    const validated = await validateRelayApiV1Contract()
    expect(validated.manifest.contract).toBe(CONTRACT)
    expect(validated.manifest.fixtures).toHaveLength(EXPECTED_FIXTURE_COUNT)
    expect(validated.directory).toBe(relayApiV1ContractDirectory)
  })

  it('rejects raw manifest or fixture hash drift', async () => {
    const manifestTamper = await copyContract()
    await writeFile(join(manifestTamper, 'manifest.json'), '{}\n', 'utf8')
    await expect(validateRelayApiV1Contract(manifestTamper)).rejects.toThrow(
      /manifest\.json must contain exactly/,
    )

    const fixtureTamper = await copyContract()
    await writeFile(join(fixtureTamper, 'server-info-v1.json'), '{}\n', 'utf8')
    await expect(validateRelayApiV1Contract(fixtureTamper)).rejects.toThrow(
      /fixture SHA-256 mismatch/,
    )
  })

  it('rejects extra and missing inventory entries', async () => {
    const extra = await copyContract()
    await writeFile(join(extra, 'extra.json'), '{}\n', 'utf8')
    await expect(validateRelayApiV1Contract(extra)).rejects.toThrow(/inventory mismatch/)

    const missing = await copyContract()
    await unlink(join(missing, 'error-v1.json'))
    await expect(validateRelayApiV1Contract(missing)).rejects.toThrow(/inventory mismatch/)
  })

  it('rejects a stray source.json (no per-checkout pin allowed inside the monorepo)', async () => {
    const directory = await copyContract()
    await writeFile(
      join(directory, 'source.json'),
      JSON.stringify({ schemaVersion: 1, contract: CONTRACT }) + '\n',
      'utf8',
    )
    await expect(validateRelayApiV1Contract(directory)).rejects.toThrow(/inventory mismatch/)
  })

  it('rejects invalid UTF-8 instead of accepting replacement characters', () => {
    expect(() => parseStrictJson(Buffer.from([0x7b, 0x22, 0xff, 0x22, 0x7d]), 'bad.json')).toThrow(
      /not valid UTF-8/,
    )
  })

  it('rejects fixture paths outside the canonical server grammar', () => {
    for (const path of ['nested/error-v1.json', 'Error-v1.json', 'error.json', '_error-v1.json']) {
      expect(() =>
        parseManifest({
          manifestVersion: 1,
          contract: CONTRACT,
          fixtures: Array.from({ length: EXPECTED_FIXTURE_COUNT }, (_, index) => ({
            path: index === 0 ? path : `fixture-${index}-v1.json`,
            sha256: '0'.repeat(64),
            mediaType: 'application/json',
            schemaVersion: 1,
          })).sort((left, right) => left.path.localeCompare(right.path)),
        }),
      ).toThrow(/unsafe path/)
    }
  })
})
