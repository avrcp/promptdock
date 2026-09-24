import { spawnSync } from 'node:child_process'
import { readFile, readdir } from 'node:fs/promises'
import { dirname, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const targets = [
  'contracts/admin-api/v2/openapi.json',
  'contracts/admin-api/v2/manifest.json',
  'contracts/admin-api/v2/inventories/routes-v2.json',
  'packages/admin-api-contract/src/generated',
]

async function files(path) {
  const absolute = resolve(root, path)
  const entries = await readdir(absolute, { withFileTypes: true }).catch(() => null)
  if (!entries) return [path]
  return (await Promise.all(entries.map((entry) =>
    entry.isDirectory() ? files(relative(root, resolve(absolute, entry.name))) : [relative(root, resolve(absolute, entry.name))],
  ))).flat().sort()
}

async function snapshot() {
  const paths = (await Promise.all(targets.map(files))).flat().sort()
  return new Map(await Promise.all(paths.map(async (path) => [path, await readFile(resolve(root, path), 'utf8')])))
}

const before = await snapshot()
const generated = spawnSync(process.execPath, ['scripts/contracts/generate-admin-api-contract.mjs'], {
  cwd: root,
  stdio: 'inherit',
})
if (generated.status !== 0) process.exit(generated.status ?? 1)
const after = await snapshot()
const changed = [...new Set([...before.keys(), ...after.keys()])].filter((path) => before.get(path) !== after.get(path))
if (changed.length > 0) {
  console.error(`Admin API generated artifacts drifted:\n${changed.map((path) => `- ${path}`).join('\n')}`)
  process.exit(1)
}
console.log(`Admin API generated artifacts are deterministic and current (${after.size} files).`)
