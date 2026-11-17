import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:https'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import tls from 'node:tls'
import {
  ConfigurationError,
  createScopedClient,
  discoverMkcertCa,
  open,
  resolveEnvironment,
  resolveTrust,
} from '../src/configuration/index.js'

function temp(t) {
  const root = mkdtempSync(join(tmpdir(), 'whalerein-config-tls-'))
  t.after(() => rmSync(root, { recursive: true, force: true }))
  return root
}

function hasOpenssl() {
  try {
    execFileSync('openssl', ['version'], { stdio: 'ignore' })
    return true
  } catch {
    return false
  }
}

function config(root, body) {
  const path = join(root, 'config.toml')
  writeFileSync(path, body)
  return path
}

function companyFile(root, caFile) {
  return config(root, `version = 1\nenvironment = "company"\n\n[environments.company]\nmode = "standalone"\n\n[environments.company.tls]\nca_provider = "file"\nca_file = ${JSON.stringify(caFile)}\n`)
}

function fakeCertificate(label) {
  return `-----BEGIN CERTIFICATE-----\n${label}\n-----END CERTIFICATE-----\n`
}

class FakeAgent {
  constructor(options) {
    this.options = options
    this.closed = false
  }

  async close() {
    this.closed = true
  }
}

function resolutionFor(path, override = null) {
  const snapshot = open({ configFile: path })
  return resolveEnvironment(snapshot, override)
}

test('config-env/AC-10/system trust never supplies an explicit CA or mutates globals', (t) => {
  const root = temp(t)
  const path = config(root, 'version = 1\n')
  const resolution = resolutionFor(path)
  const trust = resolveTrust(resolution)
  assert.deepEqual({ provider: trust.provider, ca: trust.ca, caFile: trust.caFile }, { provider: 'system', ca: null, caFile: null })
  const envBefore = process.env.NODE_TLS_REJECT_UNAUTHORIZED
  const rootsBefore = [...tls.rootCertificates]
  const client = createScopedClient(resolution, { Agent: FakeAgent })
  assert.equal(client.agent.options, undefined)
  assert.equal(process.env.NODE_TLS_REJECT_UNAUTHORIZED, envBefore)
  assert.deepEqual([...tls.rootCertificates], rootsBefore)
  assert.equal(typeof client.agent.close, 'function')
})

test('config-env/AC-10/mkcert discovery is shell-free and platform fixed', () => {
  const home = '/home/example'
  assert.equal(discoverMkcertCa({ platform: 'darwin', homeDir: home }), '/home/example/Library/Application Support/mkcert/rootCA.pem')
  assert.equal(discoverMkcertCa({ platform: 'linux', homeDir: home, envVars: {} }), '/home/example/.local/share/mkcert/rootCA.pem')
  assert.equal(discoverMkcertCa({ platform: 'linux', homeDir: home, envVars: { XDG_DATA_HOME: '/data' } }), '/data/mkcert/rootCA.pem')
  assert.equal(discoverMkcertCa({ platform: 'linux', homeDir: home, envVars: { XDG_DATA_HOME: 'relative' } }), 'relative/mkcert/rootCA.pem')
  assert.equal(discoverMkcertCa({ platform: 'win32', envVars: { LOCALAPPDATA: 'C:\\Local' } }).replaceAll('/', '\\'), 'C:\\Local\\mkcert\\rootCA.pem')
  assert.equal(discoverMkcertCa({ platform: 'win32', homeDir: home, envVars: {} }).replaceAll('/', '\\'), '\\home\\example\\AppData\\Local\\mkcert\\rootCA.pem')
  assert.throws(() => discoverMkcertCa({ platform: 'freebsd', homeDir: home }), { code: 'invalid_tls' })
})

test('config-env/AC-10/mkcert trust resolves only for built-in local and is scoped', (t) => {
  const root = temp(t)
  const mkcertDir = join(root, 'Library', 'Application Support', 'mkcert')
  mkdirSync(mkcertDir, { recursive: true })
  writeFileSync(join(mkcertDir, 'rootCA.pem'), fakeCertificate('local'))
  const local = resolutionFor(config(root, 'version = 1\nenvironment = "local"\n'))
  const trust = resolveTrust(local, { platform: 'darwin', homeDir: root })
  assert.equal(trust.provider, 'mkcert')
  assert.equal(trust.scope, 'local')
  assert.equal(trust.caFile, join(mkcertDir, 'rootCA.pem'))
  assert.equal(trust.ca.includes('BEGIN CERTIFICATE'), true)
  const production = resolutionFor(config(root, 'version = 1\nenvironment = "production"\n'))
  assert.equal(resolveTrust(production, { platform: 'darwin', homeDir: root }).provider, 'system')
  const client = createScopedClient(local, { Agent: FakeAgent, platform: 'darwin', homeDir: root })
  assert.equal(Buffer.isBuffer(client.agent.options.connect.ca), true)
  assert.equal(client.agent.options.connect.ca.includes('BEGIN CERTIFICATE'), true)
})

test('config-env/AC-10/missing or wrong mkcert CA is actionable and does not change selection', (t) => {
  const root = temp(t)
  const local = resolutionFor(config(root, 'version = 1\nenvironment = "local"\n'))
  assert.throws(() => resolveTrust(local, { platform: 'darwin', homeDir: root }), { code: 'invalid_tls', environment: 'local' })
  const mkcertDir = join(root, 'Library', 'Application Support', 'mkcert')
  mkdirSync(mkcertDir, { recursive: true })
  writeFileSync(join(mkcertDir, 'rootCA.pem'), 'not a certificate')
  assert.throws(() => resolveTrust(local, { platform: 'darwin', homeDir: root }), { code: 'invalid_tls', environment: 'local' })
  assert.equal(local.environment, 'local')
})

test('config-env/AC-10/explicit CA file resolves relative to the config parent and expands ~ once', (t) => {
  const root = temp(t)
  const relative = companyFile(root, 'certs/rootCA.pem')
  mkdirSync(join(root, 'certs'), { recursive: true })
  writeFileSync(join(root, 'certs', 'rootCA.pem'), fakeCertificate('relative'))
  assert.equal(resolveTrust(resolutionFor(relative)).caFile, realpathSync(join(root, 'certs', 'rootCA.pem')))

  const home = join(root, 'home')
  mkdirSync(join(home, 'certs'), { recursive: true })
  writeFileSync(join(home, 'certs', 'rootCA.pem'), fakeCertificate('home'))
  const fromHome = companyFile(root, '~/certs/rootCA.pem')
  assert.equal(resolveTrust(resolutionFor(fromHome), { homeDir: home }).caFile, join(home, 'certs', 'rootCA.pem'))

  const missing = companyFile(root, 'certs/missing.pem')
  assert.throws(() => resolveTrust(resolutionFor(missing)), { code: 'invalid_tls' })
  writeFileSync(join(root, 'certs', 'missing.pem'), 'not a certificate')
  assert.throws(() => resolveTrust(resolutionFor(missing)), { code: 'invalid_tls' })
})

test('config-env/AC-10/scoped clients expose fetch/close and no second environment inherits local trust', async (t) => {
  const root = temp(t)
  const local = resolutionFor(config(root, 'version = 1\nenvironment = "local"\n'))
  const mkcertDir = join(root, 'Library', 'Application Support', 'mkcert')
  mkdirSync(mkcertDir, { recursive: true })
  writeFileSync(join(mkcertDir, 'rootCA.pem'), fakeCertificate('local'))
  const localClient = createScopedClient(local, { Agent: FakeAgent, platform: 'darwin', homeDir: root })
  const productionClient = createScopedClient(resolutionFor(config(root, 'version = 1\nenvironment = "production"\n')), { Agent: FakeAgent })
  assert.equal(Buffer.isBuffer(localClient.agent.options.connect.ca), true)
  assert.equal(productionClient.agent.options, undefined)
  await productionClient.close()
  await productionClient.close()
  await localClient.close()
  assert.equal(localClient.agent.closed, true)
  await assert.rejects(localClient.fetch('https://app.whalerein.test/'), { code: 'invalid_tls' })
})

test('config-env/AC-10/real handshake accepts the scoped CA, rejects a wrong CA and wrong hostname', { skip: !hasOpenssl() ? 'openssl CLI is required to generate fixture certificates' : false }, async (t) => {
  const root = temp(t)
  const fixtures = join(root, 'pki')
  mkdirSync(fixtures)
  const run = (...args) => execFileSync('openssl', args, { cwd: fixtures, stdio: 'ignore' })
  run('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'ca.key', '-out', 'ca.pem', '-days', '3650', '-subj', '/CN=WhaleRein Test CA', '-addext', 'basicConstraints=critical,CA:TRUE', '-addext', 'keyUsage=critical,keyCertSign,cRLSign')
  run('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'ca2.key', '-out', 'ca2.pem', '-days', '3650', '-subj', '/CN=WhaleRein Second CA', '-addext', 'basicConstraints=critical,CA:TRUE', '-addext', 'keyUsage=critical,keyCertSign,cRLSign')
  writeFileSync(join(fixtures, 'ext.cnf'), '[v3]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n')
  writeFileSync(join(fixtures, 'wrong.cnf'), '[v3]\nsubjectAltName=DNS:wrong.example\nextendedKeyUsage=serverAuth\n')
  const sign = (name, ca, cnf) => {
    run('req', '-newkey', 'rsa:2048', '-nodes', '-keyout', `${name}.key`, '-out', `${name}.csr`, '-subj', '/CN=localhost')
    run('x509', '-req', '-in', `${name}.csr`, '-CA', ca, '-CAkey', ca.replace('.pem', '.key'), '-CAcreateserial', '-out', `${name}.pem`, '-days', '3650', '-extfile', cnf, '-extensions', 'v3')
  }
  sign('server', 'ca.pem', 'ext.cnf')
  sign('wrong-host', 'ca.pem', 'wrong.cnf')
  sign('server2', 'ca2.pem', 'ext.cnf')

  const serverFor = async (cert) => {
    const server = createServer({ key: readFileSync(join(fixtures, `${cert}.key`)), cert: readFileSync(join(fixtures, `${cert}.pem`)) }, (request, response) => response.end('ok'))
    await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
    t.after(() => new Promise((resolve) => {
      server.closeAllConnections?.()
      server.close(resolve)
    }))
    return `https://localhost:${server.address().port}/`
  }
  const server = await serverFor('server')
  const wrongHostServer = await serverFor('wrong-host')

  const good = resolutionFor(companyFile(root, join(fixtures, 'ca.pem')))
  const goodClient = createScopedClient(good)
  t.after(() => goodClient.close())
  const response = await goodClient.fetch(server)
  assert.equal(response.status, 200)
  assert.equal(await response.text(), 'ok')

  const badCa = resolutionFor(companyFile(root, join(fixtures, 'ca2.pem')))
  const badCaClient = createScopedClient(badCa)
  t.after(() => badCaClient.close())
  await assert.rejects(badCaClient.fetch(server), (error) => {
    assert.equal(error instanceof ConfigurationError, true)
    assert.equal(error.code, 'invalid_tls')
    return true
  })

  const wrongHost = resolutionFor(companyFile(root, join(fixtures, 'ca.pem')))
  const wrongHostClient = createScopedClient(wrongHost)
  t.after(() => wrongHostClient.close())
  await assert.rejects(wrongHostClient.fetch(wrongHostServer), { code: 'invalid_tls' })
})

test('config-env/AC-10/network failures are not misreported as trust failures', async (t) => {
  const root = temp(t)
  const path = companyFile(root, join(root, 'missing.pem'))
  mkdirSync(join(root, 'certs'), { recursive: true })
  writeFileSync(join(root, 'missing.pem'), fakeCertificate('ca'))
  const resolution = resolutionFor(path)
  const client = createScopedClient(resolution, {
    fetch: async () => { const error = new Error('connect ECONNREFUSED'); error.code = 'ECONNREFUSED'; throw error },
    Agent: FakeAgent,
  })
  await assert.rejects(client.fetch('https://localhost/'), { code: 'ECONNREFUSED' })
  await client.close()
})
