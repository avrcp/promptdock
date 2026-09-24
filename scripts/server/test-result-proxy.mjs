// Real Caddy routing/log boundary test. Upstream fixture is deliberately synthetic;
// result authorization and rendering are exercised separately by Rust HTTP tests.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url));
const image = 'caddy:2.11.4-alpine';
const name = `promptdock-result-proxy-${process.pid}`;
const temporary = await mkdtemp(join(tmpdir(), 'promptdock-result-proxy-'));
const sentinel = 'RESULT-PROXY-TOKEN-SENTINEL';
const raw = '原文\r\n```rust\nfn main() {}\n```\n  ';
const docker = (...args) => execFileSync('docker', args, { encoding: 'utf8', timeout: 60_000, stdio: ['pipe', 'pipe', 'pipe'] });
const server = createServer((req, res) => {
  if (req.url.startsWith('/v1/')) { res.writeHead(401); res.end(); return; }
  if (req.url.startsWith('/result-assets/')) { res.writeHead(200, { 'content-type': 'text/css' }); res.end('body { margin: 0; }'); return; }
  res.writeHead(200, { 'content-type': req.url.endsWith('/raw') ? 'text/plain; charset=utf-8' : 'text/html; charset=utf-8', 'cache-control': 'no-store', 'content-security-policy': "default-src 'self'; object-src 'none'; frame-ancestors 'none'", 'x-robots-tag': 'noindex, nofollow, noarchive' });
  res.end(req.method === 'HEAD' ? '' : raw);
});
let started = false;
try {
  await new Promise(resolve => server.listen(0, '0.0.0.0', resolve));
  const port = server.address().port;
  const hash = docker('run', '--rm', image, 'caddy', 'hash-password', '--plaintext', 'isolated-proxy-test').trim();
  const configPath = process.argv[2] ?? 'deploy/caddy/Caddyfile.example';
  let config = await readFile(resolve(repositoryRoot, configPath), 'utf8');
  config = config.replace(/(?:admin\.example\.com|https:\/\/(?:127\.0\.0\.1|caddy):8443) \{/, 'http://:8088 {\n\tlog')
    .replace(/\ttls \{[\s\S]*?\n\t\}/, '')
    .replace(/\ttls internal\r?\n/, '')
    .replace('bind 127.0.0.1', 'bind 0.0.0.0')
    .replaceAll('127.0.0.1:8080', `host.docker.internal:${port}`)
    .replace('{$ADMIN_USER}', 'isolated-admin')
    .replace('{$ADMIN_PASSWORD_HASH}', hash)
    .replace('<CADDY_HASH_ONLY>', hash);
  await writeFile(join(temporary, 'Caddyfile'), config);
  docker('run', '--rm', '-v', `${temporary}:/test:ro`, image, 'caddy', 'validate', '--config', '/test/Caddyfile');
  docker('run', '-d', '--name', name, '-p', '127.0.0.1::8088', '-v', `${temporary}:/test:ro`, image, 'caddy', 'run', '--config', '/test/Caddyfile');
  started = true;
  const mapping = docker('port', name, '8088/tcp').trim();
  const base = `http://${mapping}`;
  for (let attempt = 0; ; attempt++) {
    try { await fetch(base, { signal: AbortSignal.timeout(2000) }); break; }
    catch (error) { if (attempt === 30) throw error; await delay(200); }
  }
  for (const suffix of ['', '/raw']) {
    const response = await fetch(`${base}/r/${sentinel}${suffix}`);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('www-authenticate'), null);
    assert.equal(response.headers.get('cache-control'), 'no-store');
    assert.equal(response.headers.get('referrer-policy'), 'no-referrer');
    assert.equal(response.headers.get('x-content-type-options'), 'nosniff');
    assert.equal(response.headers.get('content-security-policy'), "default-src 'self'; object-src 'none'; frame-ancestors 'none'");
    assert.equal(response.headers.get('content-type'), suffix ? 'text/plain; charset=utf-8' : 'text/html; charset=utf-8');
    assert.equal(await response.text(), raw);
    const head = await fetch(`${base}/r/${sentinel}${suffix}`, { method: 'HEAD' });
    assert.equal(head.status, 200);
    assert.equal(await head.text(), '');
  }
  assert.equal((await fetch(`${base}/result-assets/viewer-v1.css`)).status, 200);
  assert.equal((await fetch(`${base}/v1/results`)).status, 401);
  for (const path of ['/', '/admin/api/v2/meta', '/assets/app.js', '/r-other']) {
    const response = await fetch(base + path);
    assert.equal(response.status, 401);
    assert.ok(response.headers.get('www-authenticate')?.startsWith('Basic'));
  }
  await new Promise(resolve => server.close(resolve));
  const failed = await fetch(`${base}/r/${sentinel}/raw`);
  assert.equal(failed.status, 502);
  await delay(300);
  const logResult = spawnSync('docker', ['logs', name], { encoding: 'utf8', timeout: 10_000 });
  assert.equal(logResult.status, 0);
  const logs = logResult.stdout + logResult.stderr;
  assert.ok(!logs.includes(sentinel), 'bearer token leaked into Caddy logs');
  console.log(`PASS (${configPath}): real Caddy validate; GET/HEAD/raw/assets routing; Admin/API isolation; headers; upstream-failure/access log sentinel. Synthetic upstream, not phone acceptance.`);
} finally {
  server.closeAllConnections();
  server.close();
  if (started) docker('rm', '-f', name);
  await rm(temporary, { recursive: true, force: true });
}
