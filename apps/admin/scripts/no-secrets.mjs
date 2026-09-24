import { readdir, readFile } from 'node:fs/promises'
import { join, relative } from 'node:path'
import process from 'node:process'

const root = process.cwd()
const ignoredDirectories = new Set([
  '.git',
  'dist',
  'node_modules',
  'playwright-report',
  'test-results',
])
const ignoredExtensions = new Set(['.png', '.jpg', '.jpeg', '.gif', '.webp', '.zip', '.map'])
const detectors = [
  { name: 'private key', pattern: /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/ },
  { name: 'AWS access key', pattern: /\bAKIA[0-9A-Z]{16}\b/ },
  { name: 'GitHub token', pattern: /\bgh[pousr]_[A-Za-z0-9_]{20,}\b/ },
  { name: 'OpenAI key', pattern: /\bsk-[A-Za-z0-9_-]{20,}\b/ },
  { name: 'real PromptDock token', pattern: /(^|[^A-Za-z0-9_-])pdv2\.[A-Za-z0-9._-]{8,}/m },
  { name: 'Bearer credential', pattern: /Authorization\s*:\s*Bearer\s+[A-Za-z0-9._-]{8,}/i },
]

async function collect(directory) {
  const entries = await readdir(directory, { withFileTypes: true })
  const paths = []
  for (const entry of entries) {
    if (entry.isDirectory()) {
      if (!ignoredDirectories.has(entry.name))
        paths.push(...(await collect(join(directory, entry.name))))
    } else if (
      !ignoredExtensions.has(entry.name.slice(entry.name.lastIndexOf('.')).toLowerCase())
    ) {
      paths.push(join(directory, entry.name))
    }
  }
  return paths
}

const matches = []
for (const file of await collect(root)) {
  let content
  try {
    content = await readFile(file, 'utf8')
  } catch {
    continue
  }
  for (const detector of detectors) {
    if (detector.pattern.test(content)) matches.push(`${relative(root, file)}: ${detector.name}`)
  }
}

if (matches.length > 0) {
  console.error('Secret scan failed:')
  for (const match of matches) console.error(`- ${match}`)
  process.exitCode = 1
} else {
  console.log('Secret scan passed: no credential signatures detected.')
}
