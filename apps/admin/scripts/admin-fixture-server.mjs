import { createServer } from 'node:http'
import { readFileSync, statSync } from 'node:fs'
import { extname, join, normalize, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const appRoot = fileURLToPath(new URL('..', import.meta.url))
const fixtureRoot = fileURLToPath(
  new URL('../../../contracts/admin-api/v2/fixtures/', import.meta.url),
)
const distRoot = resolve(appRoot, 'dist')
const port = Number(process.env.ADMIN_FIXTURE_PORT ?? process.env.ADMIN_INTEGRATION_PORT ?? 4174)
const host = process.env.ADMIN_FIXTURE_HOST ?? '127.0.0.1'

const routes = new Map([
  ['/meta', 'meta-v2.json'],
  ['/overview', 'overview-v2.json'],
  ['/devices', 'devices-page-v2.json'],
  ['/wechat/status', 'wechat-status-v2.json'],
  ['/wechat/events', 'wechat-events-page-v2.json'],
  ['/deliveries', 'deliveries-page-v2.json'],
  ['/interactive-replies', 'interactive-replies-page-v2.json'],
  ['/inbound-commands', 'inbound-commands-page-v2.json'],
  ['/system', 'system-v2.json'],
])

const LOGIN_ID = '00000000-0000-4000-8000-000000000005'
let loginState = 'idle'

const server = createServer(async (request, response) => {
  const url = new URL(request.url ?? '/', `http://${request.headers.host ?? '127.0.0.1'}`)
  if (url.pathname.startsWith('/admin/api/v2')) {
    await serveApi(request, url, response)
    return
  }
  if (request.method !== 'GET' && request.method !== 'HEAD') {
    response.writeHead(405, { Allow: 'GET, HEAD' })
    response.end()
    return
  }
  serveStatic(url.pathname, response)
})

server.listen(port, host, () => {
  console.log(`Admin fixture server listening on http://${host}:${port}`)
})

async function serveApi(request, url, response) {
  const path = url.pathname.slice('/admin/api/v2'.length)
  if (path === '/meta' && (request.method === 'GET' || request.method === 'HEAD')) {
    const meta = JSON.parse(readFixture('meta-v2.json'))
    meta.capabilities = [
      'admin_read_v2',
      'admin_device_manage_v2',
      'admin_maintenance_v2',
      'admin_wechat_manage_v2',
      'admin_wechat_login_v2',
      'admin_wechat_status_v2',
    ]
    sendJson(response, JSON.stringify(meta))
    return
  }
  if (path === '/devices' && request.method === 'POST') {
    const body = await readJsonBody(request, response)
    if (!body) return
    if (
      !isExactObject(body, ['name', 'scopes']) ||
      typeof body.name !== 'string' ||
      !Array.isArray(body.scopes) ||
      !body.scopes.every((scope) => typeof scope === 'string')
    ) {
      sendJson(response, readFixture('error-v2.json'), 422)
      return
    }
    sendJson(response, readFixture('device-create-receipt-v2.json'), 201)
    return
  }
  if (/^\/devices\/[^/]+\/rotate$/.test(path) && request.method === 'POST') {
    const body = await readJsonBody(request, response)
    if (!body) return
    if (!isExactObject(body, [])) {
      sendJson(response, readFixture('error-v2.json'), 422)
      return
    }
    sendJson(response, readFixture('device-rotate-receipt-v2.json'))
    return
  }
  if (path === '/wechat/login' && request.method === 'POST') {
    const body = await readJsonBody(request, response)
    if (!body) return
    if (!isExactObject(body, ['forceFresh']) || typeof body.forceFresh !== 'boolean') {
      sendJson(response, readFixture('error-v2.json'), 422)
      return
    }
    loginState = 'waiting_scan'
    sendJson(response, readLiveLoginFixture('wechat-login-waiting-scan-v2.json'))
    return
  }
  if (path === `/wechat/login/${LOGIN_ID}` && request.method === 'GET') {
    if (loginState === 'cancelled') {
      sendJson(response, readLiveLoginFixture('wechat-login-cancelled-v2.json'))
      return
    }
    loginState = 'verify_code_required'
    sendJson(response, readLiveLoginFixture('wechat-login-verify-required-v2.json'))
    return
  }
  if (path === `/wechat/login/${LOGIN_ID}/verify` && request.method === 'POST') {
    const body = await readJsonBody(request, response)
    if (!body) return
    if (
      !isExactObject(body, ['code']) ||
      typeof body.code !== 'string' ||
      !/^[0-9]{1,16}$/.test(body.code)
    ) {
      sendJson(response, readFixture('error-v2.json'), 422)
      return
    }
    loginState = 'verify_code_required'
    sendJson(response, readLiveLoginFixture('wechat-login-verify-required-v2.json'))
    return
  }
  if (path === `/wechat/login/${LOGIN_ID}` && request.method === 'DELETE') {
    const body = await readJsonBody(request, response)
    if (!body) return
    if (!isExactObject(body, [])) {
      sendJson(response, readFixture('error-v2.json'), 422)
      return
    }
    loginState = 'cancelled'
    sendJson(response, readLiveLoginFixture('wechat-login-cancelled-v2.json'))
    return
  }
  if (path.startsWith('/wechat/login')) {
    response.writeHead(405, { Allow: 'GET, POST, DELETE' })
    response.end()
    return
  }
  if (request.method !== 'GET' && request.method !== 'HEAD') {
    response.writeHead(405, { Allow: 'GET, HEAD' })
    response.end()
    return
  }
  if (url.pathname === '/admin/api/v2/devices/00000000-0000-4000-8000-000000000001') {
    sendJson(response, readFixture('device-detail-v2.json'))
    return
  }
  const filename = routes.get(url.pathname.slice('/admin/api/v2'.length))
  if (!filename) {
    sendJson(response, readFixture('error-v2.json'), 404)
    return
  }
  sendJson(response, readFixture(filename))
}

async function readJsonBody(request, response) {
  if (
    !String(request.headers['content-type'] ?? '')
      .toLowerCase()
      .startsWith('application/json')
  ) {
    sendJson(response, readFixture('error-v2.json'), 415)
    return null
  }
  let raw = ''
  for await (const chunk of request) raw += String(chunk)
  try {
    return JSON.parse(raw)
  } catch {
    sendJson(response, readFixture('error-v2.json'), 400)
    return null
  }
}

function isExactObject(value, keys) {
  return (
    typeof value === 'object' &&
    value !== null &&
    !Array.isArray(value) &&
    Object.keys(value).length === keys.length &&
    keys.every((key) => Object.hasOwn(value, key))
  )
}

function serveStatic(pathname, response) {
  const relative = pathname === '/' ? '/index.html' : pathname
  const requested = resolve(join(distRoot, normalize(relative)))
  const candidate = isFile(requested) ? requested : resolve(distRoot, 'index.html')
  if (!requested.startsWith(distRoot) || !candidate.startsWith(distRoot) || !isFile(candidate)) {
    response.writeHead(404, { 'Content-Type': 'text/plain' })
    response.end('Not found')
    return
  }
  const contentType =
    {
      '.html': 'text/html; charset=utf-8',
      '.js': 'text/javascript; charset=utf-8',
      '.css': 'text/css; charset=utf-8',
      '.map': 'application/json',
    }[extname(candidate)] ?? 'application/octet-stream'
  response.writeHead(200, { 'Content-Type': contentType, 'Cache-Control': 'no-store' })
  response.end(readFileSync(candidate))
}

function readFixture(filename) {
  return readFileSync(join(fixtureRoot, filename))
}

function readLiveLoginFixture(filename) {
  const session = JSON.parse(readFixture(filename))
  session.expiresAt = Date.now() + 3 * 60_000
  return JSON.stringify(session)
}

function sendJson(response, body, status = 200) {
  response.writeHead(status, {
    'Content-Type': 'application/json',
    'Cache-Control': 'no-store, max-age=0',
    'X-Content-Type-Options': 'nosniff',
    'X-Request-Id': '00000000-0000-4000-8000-000000000000',
  })
  response.end(body)
}

function isFile(path) {
  try {
    return statSync(path).isFile()
  } catch {
    return false
  }
}
