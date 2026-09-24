import { spawn } from 'node:child_process'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const workspaceRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const temporaryRoot = await mkdtemp(join(tmpdir(), 'promptdock-relay-dev-'))
const configPath = join(temporaryRoot, 'config.toml')
const databasePath = join(temporaryRoot, 'relay.db').replaceAll('\\', '/')
const connectionPath = join(temporaryRoot, 'wechat-connection.enc').replaceAll('\\', '/')

await writeFile(
  configPath,
  `[server]
bind = "127.0.0.1:8080"
exposure = "loopback"

[admin]
enabled = true
bind = "127.0.0.1:8081"
allowed_origin = "http://127.0.0.1:5173"
mode = "operator"

[database]
path = "${databasePath}"

[wechat]
enabled = false
connection_file = "${connectionPath}"
`,
  'utf8',
)

const command = (name) => (process.platform === 'win32' && name === 'corepack' ? `${name}.cmd` : name)
const children = new Set()
const exits = new Map()
let stopping = false

function start(name, executable, args) {
  const invocation = portableInvocation(executable, args)
  const child = spawn(invocation.executable, invocation.args, {
    cwd: workspaceRoot,
    env: process.env,
    stdio: 'inherit',
    windowsHide: true,
  })
  children.add(child)
  const exited = new Promise((resolveExit) => child.once('close', resolveExit))
  exits.set(child, exited)
  child.once('error', (error) => {
    console.error(`${name} failed to start: ${error.message}`)
    if (!stopping) void stop(1)
  })
  child.once('exit', (code, signal) => {
    children.delete(child)
    if (!stopping) {
      console.error(`${name} stopped (${signal ?? `exit ${code ?? 1}`}); shutting down the dev stack.`)
      void stop(code ?? 1)
    }
  })
}

function portableInvocation(executable, args) {
  return process.platform === 'win32' && executable.toLowerCase().endsWith('.cmd')
    ? {
        executable: process.env.ComSpec ?? 'cmd.exe',
        args: ['/d', '/s', '/c', executable, ...args],
      }
    : { executable, args }
}

async function stop(exitCode) {
  if (stopping) return
  stopping = true
  for (const child of children) {
    child.kill('SIGTERM')
  }
  await Promise.all(
    [...exits].map(async ([child, exited]) => {
      const timeout = setTimeout(() => {
        if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
      }, 5000)
      timeout.unref()
      await exited
      clearTimeout(timeout)
    }),
  )
  await rm(temporaryRoot, { recursive: true, force: true })
  process.exitCode = exitCode
}

process.once('SIGINT', () => void stop(0))
process.once('SIGTERM', () => void stop(0))

console.log('Relay public API: http://127.0.0.1:8080')
console.log('Relay Admin API: http://127.0.0.1:8081/admin/api/v2')
console.log('Relay Admin UI: http://127.0.0.1:5173')
console.log(`Development state: ${temporaryRoot}`)

start('Relay', command('cargo'), [
  'run',
  '-p',
  'promptdock-server',
  '--',
  'serve',
  '--config',
  configPath,
])
if (process.platform === 'win32') {
  start('Admin', process.env.ComSpec ?? 'cmd.exe', [
    '/d',
    '/s',
    '/c',
    'corepack pnpm@11.10.0 admin:dev',
  ])
} else {
  start('Admin', 'corepack', ['pnpm@11.10.0', 'admin:dev'])
}
