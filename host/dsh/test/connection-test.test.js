import assert from 'node:assert/strict'
import test from 'node:test'
import http from 'node:http'
import net from 'node:net'
import { once } from 'node:events'
import { EventEmitter } from 'node:events'
import { Context } from '@deepseek-ai/cordis'
import { testConnection, probeAccess, probeMqtt } from '../src/connection-test.js'
import { WhaleReinSettingsService } from '../src/settings.js'

async function listen(server) {
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  return server.address().port
}
async function close(server) {
  await new Promise((resolve) => server.close(resolve))
}

const credentials = { hostId: 'test-host', token: 'test-only' }

// A loopback protocol peer accepts a CONNECT; production grants and sessions
// are never used by these tests.
test('connection testing authenticates a draft without configuration writes', async () => {
  const paths = []
  const access = http.createServer((req, res) => { paths.push(req.url); res.writeHead(200); res.end() })
  let connections = 0
  const mqtt = net.createServer((socket) => {
    socket.once('data', (data) => {
      assert.equal(data[0] >> 4, 1)
      connections += 1
      socket.write(Buffer.from([0x20, 3, 0, 0, 0]))
    })
    socket.on('error', () => {})
  })
  const ctx = new Context()
  try {
    const accessPort = await listen(access)
    const mqttPort = await listen(mqtt)
    const config = { hostId: 'test-host', hostToken: 'test-only', server: '127.0.0.1', caFile: '',
      allowInsecureMqtt: true, accessUrl: `http://127.0.0.1:${accessPort}`, mqttUrl: `mqtt://127.0.0.1:${mqttPort}` }
    const host = { started: true, config, access: { connected: true } }
    const original = { ...config }
    const draft = { server: '127.0.0.1', host_id: '', host_token: '', access_port: String(accessPort), mqtt_port: String(mqttPort), ca_file: '' }
    const service = new WhaleReinSettingsService(ctx, host)
    const pending = service.testConnection(draft)
    assert.equal((await service.testConnection(draft)).configuration.code, 'busy')
    assert.deepEqual(await pending, {
      configuration: { ok: true }, access: { ok: true }, mqtt: { ok: true, authenticated: true }, running: 'connected',
    })
    assert.equal((await service.testConnection(draft)).configuration.code, 'rate_limited')
    host.access.connected = false
    assert.equal((await testConnection(host, draft)).running, 'disconnected')
    assert.equal((await testConnection(host, { ...draft, host_id: 'different' })).running, 'not_checked')
    assert.equal((await testConnection({ ...host, started: false }, draft)).running, 'not_checked')
    assert.deepEqual(config, original)
    assert.ok(paths.every((path) => path === '/readyz'))
    assert.equal(connections, 4)
  } finally {
    await Promise.all([close(access), close(mqtt)])
    await ctx.fiber.dispose()
  }
})

test('invalid drafts and CA paths are rejected before probing and do not expose input', async () => {
  for (const draft of [null, [], { unexpected: 'x' }, { server: 12 }, { server: 'x'.repeat(4097) }, { server: 'https://invalid' }]) {
    assert.deepEqual(await testConnection(null, draft), { configuration: { ok: false, code: 'invalid_config' } })
  }
  const host = { config: { hostId: 'test', hostToken: 'test-only', mqttUrl: 'mqtts://127.0.0.1:8883', accessUrl: 'https://127.0.0.1', caFile: '' } }
  assert.equal((await testConnection(host, { ca_file: '/nonexistent/whalerein-test-ca' })).configuration.ok, false)
  assert.equal((await testConnection({ config: { ...host.config, accessUrl: 'http://127.0.0.1' } }, {})).configuration.ok, false)
  assert.equal((await testConnection({ config: { ...host.config, accessUrl: 'https://user:password@127.0.0.1' } }, {})).configuration.ok, false)
})

test('when readiness returns 503 or redirects it reports not_ready, a stalled response times out, and invalid probe URLs reject invalid_config', async () => {
  let status = 503
  const sockets = new Set()
  const server = http.createServer((req, res) => {
    if (status === 0) return
    res.writeHead(status, { Location: 'http://127.0.0.1/should-not-follow' }); res.end()
  })
  server.on('connection', (socket) => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)) })
  try {
    const port = await listen(server)
    const url = `http://127.0.0.1:${port}`
    assert.equal((await probeAccess(url)).code, 'not_ready')
    status = 302
    assert.equal((await probeAccess(url)).code, 'not_ready')
    status = 0
    assert.equal((await probeAccess(url, undefined, 20)).code, 'timeout')
    assert.equal((await probeMqtt(`mqtts://127.0.0.1:${port}`, undefined, 100, credentials)).code, 'network')
  } finally {
    for (const socket of sockets) socket.destroy()
    await close(server)
  }
  assert.equal((await probeAccess('ftp://localhost')).code, 'invalid_config')
  assert.equal((await probeAccess('not a URL')).code, 'invalid_config')
  assert.equal((await probeMqtt('http://localhost')).code, 'invalid_config')
  assert.equal((await probeMqtt('not a URL')).code, 'invalid_config')
  assert.equal((await probeMqtt('mqtt://user:password@localhost')).code, 'invalid_config')
})

test('connection refusal is reported without raw socket errors', async () => {
  const server = net.createServer()
  const port = await listen(server)
  await close(server)
  assert.deepEqual(await probeAccess(`http://127.0.0.1:${port}`), { ok: false, code: 'refused' })
  assert.deepEqual(await probeMqtt(`mqtt://127.0.0.1:${port}`, undefined, 1000, credentials), { ok: false, code: 'refused' })
})

test('a stalled TLS handshake reaches its deadline and releases its socket', async () => {
  const sockets = new Set()
  const server = net.createServer((socket) => {
    sockets.add(socket)
    socket.on('error', () => {})
    socket.on('close', () => sockets.delete(socket))
  })
  try {
    const port = await listen(server)
    assert.deepEqual(await probeMqtt(`mqtts://127.0.0.1:${port}`, undefined, 20, credentials), { ok: false, code: 'timeout' })
  } finally {
    for (const socket of sockets) socket.destroy()
    await close(server)
  }
})


test('probe login uses a unique isolated identity and terminates on CONNACK', async () => {
  const identities = new Set()
  for (const outcome of ['success', 'credentials', 'tls', 'close']) {
    let options
    let ended = 0
    const client = new EventEmitter()
    client.end = (force, properties, done) => { assert.equal(force, true); ended += 1; done() }
    const connect = (url, input) => {
      options = input
      process.nextTick(() => {
        if (outcome === 'success') client.emit('connect')
        else if (outcome === 'credentials') client.emit('error', Object.assign(new Error('synthetic'), { code: 0x87 }))
        else if (outcome === 'tls') client.emit('error', Object.assign(new Error('synthetic'), { code: 'CERT_HAS_EXPIRED' }))
        else client.emit('close')
      })
      return client
    }
    const result = await probeMqtt('mqtts://example.invalid:8883', undefined, 100, credentials, connect)
    assert.deepEqual(result, outcome === 'success' ? { ok: true, authenticated: true } : { ok: false, code: outcome === 'credentials' ? 'credentials' : outcome === 'tls' ? 'tls' : 'network' })
    assert.match(options.clientId, /^whalerein-probe-[0-9a-f]{32}$/)
    assert.equal(identities.has(options.clientId), false)
    identities.add(options.clientId)
    assert.equal(options.username, 'probe:test-host')
    assert.equal(options.password, credentials.token)
    assert.equal(options.will, undefined)
    assert.equal(options.reconnectPeriod, 0)
    assert.equal(options.resubscribe, false)
    assert.equal(options.clean, true)
    assert.equal(options.properties.sessionExpiryInterval, 0)
    assert.equal(options.rejectUnauthorized, true)
    assert.equal(ended, 1)
  }
})
