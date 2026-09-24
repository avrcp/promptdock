import { randomBytes, timingSafeEqual } from 'node:crypto'
import { spawn, execFileSync } from 'node:child_process'
import { createReadStream } from 'node:fs'
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { request as httpRequest } from 'node:http'
import { createServer as createHttpsServer, request as httpsRequest } from 'node:https'
import { createServer as createNetServer } from 'node:net'
import { tmpdir } from 'node:os'
import { basename, dirname, extname, join, relative, resolve, sep } from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

import { inspectAdminDist } from './verify-admin-dist.mjs'

const root = resolve(fileURLToPath(new URL('../..', import.meta.url)))
const adminRoot = resolve(root, 'apps/admin')
const distRoot = resolve(adminRoot, 'dist')
const evidencePath = resolve(root, 'test-results/relay-admin-integration.latest.json')
const user = 'local-operator'
const password = randomBytes(24).toString('base64url')
const authorization = `Basic ${Buffer.from(`${user}:${password}`).toString('base64')}`
const checks = []

let temporaryRoot
let relay
let gateway
let activeChild
let interruptedSignal
let relayLog = ''
let result = 'FAIL'
let failure = null

const signal = (name) => {
  interruptedSignal ||= name
  activeChild?.kill('SIGTERM')
  relay?.kill('SIGTERM')
}
process.once('SIGINT', () => signal('SIGINT'))
process.once('SIGTERM', () => signal('SIGTERM'))

try {
  const [publicPort, adminPort, gatewayPort] = await reserveDistinctPorts(3)
  const origin = `https://127.0.0.1:${gatewayPort}`
  const version = await workspaceVersion()
  const commit = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim()

  await run(command('corepack'), ['pnpm@11.10.0', '--filter', '@promptdock/relay-admin', 'build:production'], {
    env: {
      VITE_BUILD_VERSION: version,
      VITE_BUILD_COMMIT: commit,
      VITE_ADMIN_API_MAJOR: '2',
      PROMPTDOCK_ALLOW_DIRTY_BUILD: '1',
    },
  })
  await inspectAdminDist(distRoot)
  checks.push(pass('current-production-admin', 'Built and inspected the production Admin SPA from the current workspace.'))

  await run(command('cargo'), ['build', '--locked', '-p', 'promptdock-server'])
  temporaryRoot = await mkdtemp(join(tmpdir(), 'promptdock-relay-admin-integration-'))
  const databasePath = slash(join(temporaryRoot, 'relay-normal.db'))
  const connectionPath = slash(join(temporaryRoot, 'wechat-connection.enc'))
  const configPath = join(temporaryRoot, 'config-normal.toml')
  await writeFile(
    configPath,
    `[server]\nbind = "127.0.0.1:${publicPort}"\nexposure = "loopback"\nshutdown_timeout_seconds = 5\n\n[admin]\nenabled = true\nbind = "127.0.0.1:${adminPort}"\nallowed_origin = "${origin}"\nmode = "operator"\n\n[database]\npath = "${databasePath}"\n\n[wechat]\nenabled = false\nconnection_file = "${connectionPath}"\n`,
    'utf8',
  )

  const executable = resolve(
    cargoTargetDirectory(),
    'debug',
    process.platform === 'win32' ? 'promptdock-relay.exe' : 'promptdock-relay',
  )
  relay = spawn(executable, ['serve', '--config', configPath], {
    cwd: root,
    env: { ...process.env },
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  })
  relay.stdout.on('data', captureRelayLog)
  relay.stderr.on('data', captureRelayLog)
  relay.once('error', captureRelayLog)

  const tls = await createEphemeralTls(temporaryRoot)
  gateway = createSameOriginGateway({ adminPort, tls })
  await listen(gateway, gatewayPort)
  await waitForReady(gatewayPort)
  checks.push(pass('isolated-current-relay', 'Started the current Relay build with temporary config, database, and OS-assigned ports.'))

  await verify(gatewayPort, origin)

  await closeServer(gateway)
  gateway = undefined
  await stopChild(relay)
  relay = undefined
  relayLog = ''

  const notificationDatabasePath = slash(join(temporaryRoot, 'relay-notification-only.db'))
  const notificationConfigPath = join(temporaryRoot, 'config-notification-only.toml')
  await writeFile(
    notificationConfigPath,
    `[server]\nbind = "127.0.0.1:${publicPort}"\nexposure = "loopback"\nnotification_only = true\nshutdown_timeout_seconds = 5\n\n[admin]\nenabled = true\nbind = "127.0.0.1:${adminPort}"\nallowed_origin = "${origin}"\nmode = "operator"\n\n[database]\npath = "${notificationDatabasePath}"\n\n[wechat]\nenabled = false\nconnection_file = "${connectionPath}"\n`,
    'utf8',
  )
  relay = spawn(executable, ['serve', '--config', notificationConfigPath], {
    cwd: root,
    env: { ...process.env },
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  })
  relay.stdout.on('data', captureRelayLog)
  relay.stderr.on('data', captureRelayLog)
  relay.once('error', captureRelayLog)
  gateway = createSameOriginGateway({ adminPort, tls })
  await listen(gateway, gatewayPort)
  await waitForReady(gatewayPort)
  checks.push(pass('isolated-notification-only-relay', 'Started a second real Relay process with notification_only=true and isolated durable state.'))

  await verifyNotificationOnly({ adminPort: gatewayPort, publicPort, origin })
  result = 'PASS'
} catch (error) {
  failure = safeError(error)
  console.error(`Relay/Admin integration failed: ${failure}`)
} finally {
  const cleanupFailures = []
  if (gateway) {
    try {
      await closeServer(gateway)
    } catch (error) {
      cleanupFailures.push(`gateway cleanup: ${safeError(error)}`)
    }
  }
  if (relay) {
    try {
      await stopChild(relay)
    } catch (error) {
      cleanupFailures.push(`Relay cleanup: ${safeError(error)}`)
    }
  }
  if (temporaryRoot) await rm(temporaryRoot, { recursive: true, force: true })
  if (cleanupFailures.length) {
    result = 'FAIL'
    failure = [failure, ...cleanupFailures].filter(Boolean).join('; ')
  }
  await writeEvidence()
}

if (result !== 'PASS') process.exitCode = 1

async function verify(port, origin) {
  const unauthenticated = await request(port, '/')
  equal(unauthenticated.status, 401, 'unauthenticated SPA request must return 401')
  includes(header(unauthenticated, 'www-authenticate').toLowerCase(), 'basic', 'Basic challenge missing')
  equal((await request(port, '/admin/api/v2/meta')).status, 401, 'unauthenticated Admin API request must return 401')
  equal((await request(port, '/devices/deep-link')).status, 401, 'unauthenticated SPA deep link must return 401')

  const spa = await request(port, '/', { authenticated: true })
  equal(spa.status, 200, 'authenticated SPA request must return 200')
  includes(spa.body, '<div id="app">', 'production SPA entrypoint missing')
  assertSecurityHeaders(spa)
  const fallback = await request(port, '/devices/deep-link', { authenticated: true })
  equal(fallback.status, 200, 'SPA deep link must return 200')
  includes(fallback.body, '<div id="app">', 'SPA fallback did not serve index.html')
  checks.push(pass('tls-basic-auth-spa', 'Equivalent same-origin TLS gateway enforced Basic Auth and served the SPA fallback with production headers.'))

  const meta = await request(port, '/admin/api/v2/meta', { authenticated: true })
  equal(meta.status, 200, 'Admin metadata must return 200')
  equal(meta.headers['access-control-allow-origin'], undefined, 'CORS must not be enabled')
  equal(meta.headers['access-control-allow-credentials'], undefined, 'CORS credentials must not be enabled')
  const metadata = json(meta, 'metadata')
  equal(metadata.adminApiVersion, 2, 'Admin API major must be v2')
  for (const capability of ['admin_read_v2', 'admin_device_manage_v2', 'admin_wechat_status_v2', 'admin_maintenance_v2']) {
    includes(metadata.capabilities, capability, `metadata is missing ${capability}`)
  }
  notIncludes(metadata.capabilities, 'admin_wechat_manage_v2', 'disabled WeChat must not claim manage capability')
  notIncludes(metadata.capabilities, 'admin_wechat_login_v2', 'disabled WeChat must not claim login capability')
  const wechat = json(await request(port, '/admin/api/v2/wechat/status', { authenticated: true }), 'WeChat status')
  if (wechat.state === 'connected') throw new Error('disabled WeChat unexpectedly reports connected')
  checks.push(pass('metadata-wechat-disabled-no-cors', 'Admin v2 metadata and WeChat-disabled capabilities were served without CORS headers.'))

  const accepted = await request(port, '/admin/api/v2/inbound-commands?command=list_jobs', { authenticated: true })
  equal(accepted.status, 200, 'list_jobs must be accepted')
  if (!Array.isArray(json(accepted, 'list_jobs').items)) throw new Error('list_jobs response must contain items')
  const legacy = await request(port, '/admin/api/v2/inbound-commands?command=list_runs', { authenticated: true })
  equal(legacy.status, 400, 'legacy list_runs must be rejected')
  json(legacy, 'legacy rejection')
  checks.push(pass('closed-command-contract', 'Relay accepted list_jobs and rejected the removed list_runs command.'))

  const created = await mutation(port, origin, '/admin/api/v2/devices', {
    name: `relay-integration-${Date.now()}`,
    scopes: ['job:query'],
  })
  equal(created.status, 201, 'device creation must return 201')
  const receipt = json(created, 'device creation')
  exactKeys(receipt, ['action', 'deviceId', 'issuedAt', 'oneTimeToken', 'receiptId'], 'device receipt')
  equal(receipt.action, 'create', 'device receipt action must be create')
  if (typeof receipt.oneTimeToken !== 'string' || receipt.oneTimeToken.length < 8) throw new Error('one-time device token missing')
  let oneTimeToken = receipt.oneTimeToken
  try {
    const page = json(await request(port, '/admin/api/v2/devices?limit=10', { authenticated: true }), 'device list')
    const detail = json(await request(port, `/admin/api/v2/devices/${encodeURIComponent(receipt.deviceId)}`, { authenticated: true }), 'device detail')
    equal(page.items?.some((device) => device?.id === receipt.deviceId), true, 'created device missing from list')
    equal(detail.id, receipt.deviceId, 'created device detail mismatch')
    includes(detail.scopes, 'job:query', 'device scope missing')
    if (JSON.stringify(page).includes(oneTimeToken) || JSON.stringify(detail).includes(oneTimeToken)) throw new Error('one-time token reappeared in read model')
  } finally {
    oneTimeToken = ''
  }
  checks.push(pass('device-mutation-one-time-token', 'Device creation returned a closed receipt and did not retain its one-time credential.'))

  const retention = await mutation(port, origin, '/admin/api/v2/maintenance/retention', {})
  equal(retention.status, 200, 'retention must return 200')
  const retentionReceipt = json(retention, 'retention')
  exactKeys(retentionReceipt, ['action', 'completedAt', 'inboundDeleted', 'outboxDeleted', 'receiptId', 'selectionDeleted', 'startedAt'], 'retention receipt')
  equal(retentionReceipt.action, 'maintenance.retention', 'retention receipt action mismatch')
  checks.push(pass('operator-retention', 'Operator retention mutation completed with the closed v2 receipt shape.'))
}

async function verifyNotificationOnly({ adminPort, publicPort, origin }) {
  const meta = json(await request(adminPort, '/admin/api/v2/meta', { authenticated: true }), 'notification-only metadata')
  for (const capability of ['admin_read_v2', 'admin_device_manage_v2', 'admin_wechat_status_v2', 'admin_maintenance_v2']) {
    includes(meta.capabilities, capability, `notification-only metadata is missing ${capability}`)
  }

  const ownerReceipt = json(await mutation(adminPort, origin, '/admin/api/v2/devices', {
    name: `notification-owner-${Date.now()}`,
    scopes: ['notify:write', 'notify:read_own'],
  }), 'notification owner')
  const foreignReceipt = json(await mutation(adminPort, origin, '/admin/api/v2/devices', {
    name: `notification-foreign-${Date.now()}`,
    scopes: ['notify:read_own'],
  }), 'notification foreign reader')
  const insufficientReceipt = json(await mutation(adminPort, origin, '/admin/api/v2/devices', {
    name: `notification-insufficient-${Date.now()}`,
    scopes: ['notify:write'],
  }), 'notification insufficient reader')
  const ownerAuthorization = `Bearer ${ownerReceipt.oneTimeToken}`
  const foreignAuthorization = `Bearer ${foreignReceipt.oneTimeToken}`
  const insufficientAuthorization = `Bearer ${insufficientReceipt.oneTimeToken}`

  const serverInfoResponse = await publicRequest(publicPort, '/v1/server-info', {
    headers: { Authorization: ownerAuthorization },
  })
  equal(serverInfoResponse.status, 200, 'notification-only server-info must return 200')
  const serverInfo = json(serverInfoResponse, 'notification-only server-info')
  exactKeys(serverInfo, ['apiVersion', 'features', 'serverVersion', 'wechatProtocolReference'], 'notification-only server-info')
  equal(
    JSON.stringify(serverInfo.features),
    JSON.stringify(['notifications', 'device_status_v1', 'device_scopes_v1', 'wechat_handoff_preflight_v1']),
    'notification-only server-info feature projection mismatch',
  )
  const gatewayResponse = await publicRequest(publicPort, '/v5/gateway/ws', {
    headers: { Authorization: ownerAuthorization },
  })
  equal(gatewayResponse.status, 404, 'notification-only Gateway must return 404')
  checks.push(pass('notification-only-public-capabilities', 'Public server-info omitted every remote capability and Gateway v5 returned 404.'))

  const deviceStatusResponse = await publicRequest(publicPort, '/v1/device-status', {
    headers: { Authorization: insufficientAuthorization },
  })
  equal(deviceStatusResponse.status, 200, 'self permissions must not require notify:read_own')
  const deviceStatus = json(deviceStatusResponse, 'device status')
  exactKeys(deviceStatus, ['deviceId', 'canSubmit', 'canReadOwn', 'channelState'], 'device status')
  equal(deviceStatus.canSubmit, true, 'write permission must be reported independently')
  equal(deviceStatus.canReadOwn, false, 'missing read permission must not be fabricated')
  equal(deviceStatus.channelState, 'unknown', 'channel details must be hidden without channel scope')
  checks.push(pass('notification-only-self-permissions', 'Self-status independently reported write/read scopes without sending a notification.'))

  const now = Date.now()
  const notificationId = `notification-only-${now}`
  const acceptedResponse = await publicRequest(publicPort, '/v1/notifications', {
    method: 'POST',
    headers: { Authorization: ownerAuthorization },
    body: {
      schemaVersion: 1,
      notificationId,
      dedupeKey: `notification-only-dedupe-${now}`,
      kind: 'test',
      priority: 100,
      title: 'Notification-only integration',
      body: 'Durable local notification evidence',
      correlationKey: `notification-only-correlation-${now}`,
      createdAt: now,
      expiresAt: now + 300_000,
    },
  })
  equal(acceptedResponse.status, 202, 'notification-only POST must return 202')
  const accepted = json(acceptedResponse, 'notification-only acceptance')
  equal(accepted.notificationId, notificationId, 'accepted notification id mismatch')
  equal(accepted.relayStatus, 'accepted', 'notification relay status mismatch')

  const ownStatusResponse = await publicRequest(publicPort, `/v1/notifications/${encodeURIComponent(notificationId)}`, {
    headers: { Authorization: ownerAuthorization },
  })
  equal(ownStatusResponse.status, 200, 'owner notification status must return 200')
  const ownStatus = json(ownStatusResponse, 'owner notification status')
  equal(ownStatus.notificationId, notificationId, 'owner notification status id mismatch')
  if (typeof ownStatus.status !== 'string' || ownStatus.status.length === 0) throw new Error('owner notification status is missing')

  const forbidden = await publicRequest(publicPort, `/v1/notifications/${encodeURIComponent(notificationId)}`, {
    headers: { Authorization: insufficientAuthorization },
  })
  equal(forbidden.status, 403, 'device without notify:read_own must receive 403')
  equal(json(forbidden, 'insufficient-scope status').error?.code, 'INSUFFICIENT_SCOPE', 'insufficient-scope error mismatch')
  const hidden = await publicRequest(publicPort, `/v1/notifications/${encodeURIComponent(notificationId)}`, {
    headers: { Authorization: foreignAuthorization },
  })
  equal(hidden.status, 404, 'foreign notify:read_own device must receive 404')
  equal(json(hidden, 'cross-device status').error?.code, 'NOT_FOUND', 'cross-device error mismatch')
  checks.push(pass('notification-only-device-outbox', 'A minimal device received POST 202 and its own durable status; unauthorized and cross-device reads returned 403 and 404.'))

  const system = json(await request(adminPort, '/admin/api/v2/system', { authenticated: true }), 'notification-only system')
  const worker = (name) => system.workers?.find((candidate) => candidate?.name === name)
  equal(worker('gateway-supervisor')?.state, 'disabled', 'Gateway Admin health must be disabled')
  equal(worker('inbound-worker')?.state, 'disabled', 'Inbound Admin health must be disabled')
  const inbound = json(await request(adminPort, '/admin/api/v2/inbound-commands?command=list_jobs', { authenticated: true }), 'notification-only inbound Admin')
  if (!Array.isArray(inbound.items)) throw new Error('notification-only Admin inbound list must contain items')
  const retention = await mutation(adminPort, origin, '/admin/api/v2/maintenance/retention', {})
  equal(retention.status, 200, 'notification-only Admin retention must return 200')
  equal(json(retention, 'notification-only retention').action, 'maintenance.retention', 'notification-only retention receipt mismatch')
  checks.push(pass('notification-only-admin', 'Full operator Admin remained available while Gateway and Inbound workers reported disabled.'))
}

function createSameOriginGateway({ adminPort, tls }) {
  return createHttpsServer(tls, async (incoming, outgoing) => {
    try {
      applySecurityHeaders(outgoing)
      if (!timingSafeAuthorization(incoming.headers.authorization)) {
        outgoing.writeHead(401, { 'WWW-Authenticate': 'Basic realm="PromptDock Relay Admin"' })
        outgoing.end()
        return
      }
      const url = new URL(incoming.url ?? '/', 'https://127.0.0.1')
      if (url.pathname.startsWith('/admin/api/v2/')) {
        outgoing.setHeader('Cache-Control', 'no-store, max-age=0')
        proxyAdmin(incoming, outgoing, adminPort)
        return
      }
      if (incoming.method !== 'GET' && incoming.method !== 'HEAD') {
        outgoing.writeHead(405, { Allow: 'GET, HEAD' })
        outgoing.end()
        return
      }
      const candidate = resolve(distRoot, `.${decodeURIComponent(url.pathname)}`)
      const safeCandidate = candidate === distRoot || candidate.startsWith(`${distRoot}${sep}`)
      let file = safeCandidate ? candidate : resolve(distRoot, 'index.html')
      try {
        if ((await stat(file)).isDirectory()) file = join(file, 'index.html')
      } catch {
        file = resolve(distRoot, 'index.html')
      }
      if (basename(file) === 'index.html') outgoing.setHeader('Cache-Control', 'no-cache, max-age=0, must-revalidate')
      else if (file.includes(`${sep}assets${sep}`)) outgoing.setHeader('Cache-Control', 'public, max-age=31536000, immutable')
      outgoing.setHeader('Content-Type', contentType(file))
      outgoing.writeHead(200)
      if (incoming.method === 'HEAD') outgoing.end()
      else createReadStream(file).on('error', (error) => outgoing.destroy(error)).pipe(outgoing)
    } catch (error) {
      outgoing.writeHead(500)
      outgoing.end(safeError(error))
    }
  })
}

function proxyAdmin(incoming, outgoing, port) {
  const headers = { ...incoming.headers, host: `127.0.0.1:${port}` }
  delete headers.authorization
  const upstream = httpRequest({ hostname: '127.0.0.1', port, path: incoming.url, method: incoming.method, headers }, (response) => {
    for (const [name, value] of Object.entries(response.headers)) {
      if (value !== undefined && !['connection', 'keep-alive', 'transfer-encoding', 'access-control-allow-origin', 'access-control-allow-credentials'].includes(name)) outgoing.setHeader(name, value)
    }
    outgoing.writeHead(response.statusCode ?? 502)
    response.pipe(outgoing)
  })
  upstream.setTimeout(10_000, () => upstream.destroy(new Error('Relay Admin request timed out')))
  upstream.on('error', (error) => outgoing.destroy(error))
  incoming.pipe(upstream)
}

function request(port, path, { authenticated = false, method = 'GET', body, headers = {} } = {}) {
  const requestHeaders = { ...headers }
  if (authenticated) requestHeaders.Authorization = authorization
  let encoded
  if (body !== undefined) {
    encoded = JSON.stringify(body)
    requestHeaders['Content-Type'] = 'application/json'
    requestHeaders['Content-Length'] = Buffer.byteLength(encoded)
  }
  return new Promise((resolvePromise, reject) => {
    const client = httpsRequest({ hostname: '127.0.0.1', port, path, method, rejectUnauthorized: false, headers: requestHeaders }, (response) => {
      const chunks = []
      response.on('data', (chunk) => chunks.push(Buffer.from(chunk)))
      response.on('error', reject)
      response.on('end', () => resolvePromise({ status: response.statusCode ?? 0, headers: response.headers, body: Buffer.concat(chunks).toString('utf8') }))
    })
    client.setTimeout(10_000, () => client.destroy(new Error(`request timed out: ${path}`)))
    client.on('error', reject)
    if (encoded !== undefined) client.write(encoded)
    client.end()
  })
}

function publicRequest(port, path, { method = 'GET', body, headers = {} } = {}) {
  const requestHeaders = { ...headers }
  let encoded
  if (body !== undefined) {
    encoded = JSON.stringify(body)
    requestHeaders['Content-Type'] = 'application/json'
    requestHeaders['Content-Length'] = Buffer.byteLength(encoded)
  }
  return new Promise((resolvePromise, reject) => {
    const client = httpRequest({ hostname: '127.0.0.1', port, path, method, headers: requestHeaders }, (response) => {
      const chunks = []
      response.on('data', (chunk) => chunks.push(Buffer.from(chunk)))
      response.on('error', reject)
      response.on('end', () => resolvePromise({ status: response.statusCode ?? 0, headers: response.headers, body: Buffer.concat(chunks).toString('utf8') }))
    })
    client.setTimeout(10_000, () => client.destroy(new Error(`public request timed out: ${path}`)))
    client.on('error', reject)
    if (encoded !== undefined) client.write(encoded)
    client.end()
  })
}

function mutation(port, origin, path, body) {
  return request(port, path, { authenticated: true, method: 'POST', body, headers: { Origin: origin, 'X-PromptDock-Admin-Action': '1', 'Sec-Fetch-Site': 'same-origin' } })
}

async function waitForReady(port) {
  let last = 'no response'
  for (let attempt = 0; attempt < 60; attempt += 1) {
    if (interruptedSignal) throw new Error(`integration interrupted by ${interruptedSignal}`)
    if (relay?.exitCode !== null) throw new Error(`Relay exited before readiness (${relay.exitCode}); ${relayLog.slice(-1000)}`)
    try {
      const response = await request(port, '/admin/api/v2/meta', { authenticated: true })
      if (response.status === 200) return
      last = `HTTP ${response.status}`
    } catch (error) {
      last = safeError(error)
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 500))
  }
  throw new Error(`Relay Admin did not become ready: ${last}; ${relayLog.slice(-1000)}`)
}

async function createEphemeralTls(directory) {
  const pfx = join(directory, 'gateway.pfx')
  const passphrase = randomBytes(18).toString('base64url')
  if (process.platform === 'win32') {
    const script = `$rsa=[System.Security.Cryptography.RSA]::Create(2048);$req=[System.Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=127.0.0.1',$rsa,[System.Security.Cryptography.HashAlgorithmName]::SHA256,[System.Security.Cryptography.RSASignaturePadding]::Pkcs1);$san=[System.Security.Cryptography.X509Certificates.SubjectAlternativeNameBuilder]::new();$san.AddIpAddress([System.Net.IPAddress]::Parse('127.0.0.1'));$req.CertificateExtensions.Add($san.Build());$cert=$req.CreateSelfSigned([DateTimeOffset]::UtcNow.AddMinutes(-1),[DateTimeOffset]::UtcNow.AddHours(2));[IO.File]::WriteAllBytes($env:PROMPTDOCK_INTEGRATION_PFX,$cert.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Pfx,$env:PROMPTDOCK_INTEGRATION_PFX_PASSWORD));$cert.Dispose();$rsa.Dispose()`
    await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
      capture: true,
      env: {
        PROMPTDOCK_INTEGRATION_PFX: pfx,
        PROMPTDOCK_INTEGRATION_PFX_PASSWORD: passphrase,
      },
    })
    return { pfx: await readFile(pfx), passphrase }
  }
  const key = join(directory, 'gateway-key.pem')
  const cert = join(directory, 'gateway-cert.pem')
  await run('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', key, '-out', cert, '-days', '1', '-subj', '/CN=127.0.0.1', '-addext', 'subjectAltName=IP:127.0.0.1'], { capture: true })
  return { key: await readFile(key), cert: await readFile(cert) }
}

async function reserveDistinctPorts(count) {
  const ports = []
  while (ports.length < count) {
    const port = await new Promise((resolvePromise, reject) => {
      const server = createNetServer()
      server.once('error', reject)
      server.listen(0, '127.0.0.1', () => {
        const address = server.address()
        server.close((error) => error ? reject(error) : resolvePromise(address.port))
      })
    })
    if (!ports.includes(port)) ports.push(port)
  }
  return ports
}

async function workspaceVersion() {
  const cargo = await readFile(resolve(root, 'Cargo.toml'), 'utf8')
  const match = cargo.match(/^version\s*=\s*"([^"]+)"\s*$/m)
  if (!match) throw new Error('Cargo workspace version is missing')
  return match[1]
}

function cargoTargetDirectory() {
  const metadata = execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], {
    cwd: root,
    encoding: 'utf8',
  })
  const targetDirectory = JSON.parse(metadata).target_directory
  if (typeof targetDirectory !== 'string' || targetDirectory.length === 0) {
    throw new Error('Cargo metadata did not report target_directory')
  }
  return targetDirectory
}

function run(executable, args, { env = {}, capture = false } = {}) {
  if (interruptedSignal) return Promise.reject(new Error(`integration interrupted by ${interruptedSignal}`))
  return new Promise((resolvePromise, reject) => {
    const invocation = portableInvocation(executable, args)
    const child = spawn(invocation.executable, invocation.args, { cwd: root, env: { ...process.env, ...env }, stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit', windowsHide: true })
    activeChild = child
    let stdout = ''
    let stderr = ''
    child.stdout?.on('data', (chunk) => (stdout += String(chunk)))
    child.stderr?.on('data', (chunk) => (stderr += String(chunk)))
    child.once('error', reject)
    child.once('close', (code) => {
      if (activeChild === child) activeChild = undefined
      if (code === 0) resolvePromise({ stdout, stderr })
      else reject(new Error(`${basename(executable)} ${args[0] ?? ''} failed (${code}): ${stderr.slice(-1500)}`))
    })
  })
}

async function stopChild(child) {
  if (child.exitCode !== null) return
  const closed = new Promise((resolvePromise) => child.once('close', resolvePromise))
  child.kill('SIGTERM')
  const graceful = await Promise.race([
    closed.then(() => true),
    new Promise((resolvePromise) => setTimeout(() => resolvePromise(false), 6000)),
  ])
  if (graceful) return
  if (child.exitCode === null) child.kill('SIGKILL')
  const killed = await Promise.race([
    closed.then(() => true),
    new Promise((resolvePromise) => setTimeout(() => resolvePromise(false), 3000)),
  ])
  if (!killed) throw new Error('Relay process did not exit after SIGKILL')
}

function listen(server, port) {
  return new Promise((resolvePromise, reject) => {
    server.once('error', reject)
    server.listen(port, '127.0.0.1', resolvePromise)
  })
}

function closeServer(server) {
  return new Promise((resolvePromise, reject) => server.close((error) => error ? reject(error) : resolvePromise()))
}

async function writeEvidence() {
  await mkdir(dirname(evidencePath), { recursive: true })
  const evidence = {
    schemaVersion: 1,
    scope: 'local-current-workspace-relay-admin',
    generatedAt: new Date().toISOString(),
    result,
    source: { commit: safeGitCommit(), dirtyTreeAllowed: true, siblingSourceUsed: false },
    gateway: 'node-https-basic-auth-same-origin-equivalent',
    checks,
    unverifiedScopes: { externalDeployment: 'NOT_RUN', liveWechat: 'NOT_RUN_DISABLED_BY_TEST' },
    failure: failure ?? null,
  }
  await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, 'utf8')
  console.log(`Local integration evidence: ${slash(relative(root, evidencePath))}`)
  console.log(JSON.stringify({ result, checks: checks.map(({ id, status }) => ({ id, status })) }, null, 2))
}

function safeGitCommit() {
  try { return execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim() } catch { return 'unknown' }
}

function timingSafeAuthorization(value) {
  if (typeof value !== 'string') return false
  const actual = Buffer.from(value)
  const expected = Buffer.from(authorization)
  return actual.length === expected.length && timingSafeEqual(actual, expected)
}

function applySecurityHeaders(response) {
  response.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'")
  response.setHeader('Cross-Origin-Opener-Policy', 'same-origin')
  response.setHeader('Cross-Origin-Resource-Policy', 'same-origin')
  response.setHeader('Permissions-Policy', 'camera=(), microphone=(), geolocation=()')
  response.setHeader('Referrer-Policy', 'no-referrer')
  response.setHeader('Strict-Transport-Security', 'max-age=31536000')
  response.setHeader('X-Content-Type-Options', 'nosniff')
}

function assertSecurityHeaders(response) {
  includes(header(response, 'content-security-policy'), "default-src 'self'", 'CSP missing')
  equal(header(response, 'x-content-type-options'), 'nosniff', 'nosniff missing')
  equal(header(response, 'strict-transport-security'), 'max-age=31536000', 'HSTS missing')
}

function contentType(file) {
  return ({ '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml' })[extname(file)] ?? 'application/octet-stream'
}

function json(response, label) {
  if (!/^application\/json/i.test(header(response, 'content-type'))) throw new Error(`${label} response must be JSON`)
  try { return JSON.parse(response.body) } catch { throw new Error(`${label} response contains invalid JSON`) }
}

function header(response, name) {
  const value = response.headers[name]
  if (typeof value === 'string') return value
  if (Array.isArray(value)) return value.join(', ')
  throw new Error(`missing response header: ${name}`)
}

function exactKeys(value, expected, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${label} must be an object`)
  equal(JSON.stringify(Object.keys(value).sort()), JSON.stringify([...expected].sort()), `${label} has an unexpected shape`)
}

function equal(actual, expected, message) { if (actual !== expected) throw new Error(`${message}; got ${String(actual)}`) }
function includes(values, expected, message) { if (!values?.includes(expected)) throw new Error(message) }
function notIncludes(values, expected, message) { if (values?.includes(expected)) throw new Error(message) }
function pass(id, summary) { return { id, status: 'PASS', summary } }
function slash(value) { return value.replaceAll('\\', '/') }
function command(name) { return process.platform === 'win32' && name === 'corepack' ? `${name}.cmd` : name }
function portableInvocation(executable, args) {
  return process.platform === 'win32' && executable.toLowerCase().endsWith('.cmd')
    ? { executable: process.env.ComSpec ?? 'cmd.exe', args: ['/d', '/s', '/c', executable, ...args] }
    : { executable, args }
}
function captureRelayLog(value) { relayLog = `${relayLog}${String(value)}`.slice(-20_000) }
function safeError(error) {
  return (error instanceof Error ? error.message : String(error))
    .replace(/pdv2\.[A-Za-z0-9._-]+/g, '[REDACTED_DEVICE_TOKEN]')
    .replace(/\bBasic\s+[A-Za-z0-9+/=_-]+/gi, 'Basic [REDACTED]')
    .slice(0, 2000)
}
