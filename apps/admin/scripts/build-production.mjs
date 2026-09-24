import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { isReleaseVersion } from './release-version.mjs'

const workspaceRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const cargo = readFileSync(resolve(workspaceRoot, 'Cargo.toml'), 'utf8')
const workspaceVersion = cargo.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1]
const version = process.env.VITE_BUILD_VERSION ?? workspaceVersion
const commit = process.env.VITE_BUILD_COMMIT ?? sourceCommit()
const adminApiMajor = process.env.VITE_ADMIN_API_MAJOR ?? '2'

if (typeof version !== 'string' || !isReleaseVersion(version)) {
  console.error('VITE_BUILD_VERSION must be a release version such as 0.6.0-rc.1.')
  process.exit(2)
}
if (!/^[0-9a-f]{40}$/.test(commit) || /^0{40}$/.test(commit)) {
  console.error('VITE_BUILD_COMMIT must be a full lowercase commit.')
  process.exit(2)
}
if (adminApiMajor !== '2') {
  console.error('VITE_ADMIN_API_MAJOR must match the frozen Admin API major 2.')
  process.exit(2)
}

assertAttestedSource()

try {
  const command = process.platform === 'win32' ? (process.env.ComSpec ?? 'cmd.exe') : 'pnpm'
  const args = process.platform === 'win32' ? ['/d', '/s', '/c', 'pnpm.cmd build'] : ['build']
  execFileSync(command, args, {
    stdio: 'inherit',
    env: {
      ...process.env,
      VITE_DATA_SOURCE_MODE: 'production',
      VITE_BUILD_VERSION: version,
      VITE_BUILD_COMMIT: commit,
      VITE_ADMIN_API_MAJOR: adminApiMajor,
    },
  })
} catch (error) {
  process.exit(typeof error?.status === 'number' ? error.status : 1)
}

function sourceCommit() {
  try {
    return execFileSync('git', ['rev-parse', '--verify', 'HEAD'], {
      cwd: workspaceRoot,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim()
  } catch {
    return ''
  }
}

function assertAttestedSource() {
  if (process.env.PROMPTDOCK_ALLOW_DIRTY_BUILD === '1') return
  if (process.env.PROMPTDOCK_BUILD_SOURCE_ARCHIVE === '1') {
    if (!process.env.VITE_BUILD_COMMIT) {
      console.error('Archived production builds require an explicit VITE_BUILD_COMMIT.')
      process.exit(2)
    }
    return
  }
  try {
    const status = execFileSync('git', ['status', '--porcelain', '--untracked-files=all'], {
      cwd: workspaceRoot,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim()
    if (status) {
      console.error('Production builds require a clean source tree.')
      process.exit(2)
    }
  } catch (error) {
    if (typeof error?.status === 'number') process.exit(error.status)
    console.error('Production build source identity could not be verified.')
    process.exit(2)
  }
}
