import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { ed25519, x25519 } from '@noble/curves/ed25519.js'
import { MemoryHostCredentials } from '../src/security/credentials.js'
import { CREDENTIAL_PURPOSES, ProfileCredentialStore } from '../src/credentials.js'
import {
  AUTHORIZATION_PREFIX,
  DEFAULT_DEADLINE_MS,
  ENROLLMENT_PROOF_PREFIX,
  HOST_PROOF_PREFIX,
  INTENT_PROOF_PREFIX,
  IntentJournal,
  ManagedEnrollmentClient,
  canonicalAuthorizationInput,
  canonicalIntentProof,
  createHostProof,
  enrollmentProofInput,
  fetchServiceMetadata,
  hostProofText,
} from '../src/enrollment.js'
import { validateServiceMetadata } from '../src/services.js'

const ACCESS_ORIGIN = 'https://access.example'
const NOW = 1_800_000_000_000
const HOST_ID = 'host-test-1'
const PROFILE_ID = 'c'.repeat(32)
const b64 = (value) => Buffer.from(value).toString('base64url')
const signingPrivateKey = ed25519.utils.randomSecretKey()
const signingPublicKey = ed25519.getPublicKey(signingPrivateKey)
const noisePrivateKey = x25519.utils.randomSecretKey()
const noisePublicKey = x25519.getPublicKey(noisePrivateKey)

function metadata(overrides = {}) {
  return validateServiceMetadata({
    service: 'whalerein-cloud',
    environment: 'production',
    protocol: { version: 1, capabilities: ['host-enrollment-v1'] },
    approval: { origin: 'https://app.whalerein.com', path: '/approve/{id}' },
    enrollment: { createPath: '/api/enrollment-requests', pollAfterMs: 2000 },
    ...overrides,
  })
}

function response(status, body, headers = {}) {
  const normalized = Object.fromEntries(Object.entries(headers).map(([key, value]) => [key.toLowerCase(), value]))
  return {
    status,
    redirected: false,
    headers: { get: (key) => normalized[key.toLowerCase()] ?? null },
    async json() {
      return body
    },
  }
}

function clientFixture({ fetch: fetchImpl, metadata: meta = metadata(), credentials = new MemoryHostCredentials(), stateDir, now = () => NOW, sleep = async () => {}, verifyAuthorization = null, expectedIssuer = '' } = {}) {
  const root = stateDir ?? mkdtempSync(join(tmpdir(), 'whalerein-enrollment-'))
  const store = new ProfileCredentialStore({ credentials, profileId: PROFILE_ID })
  const journal = new IntentJournal({ stateDir: root })
  const client = new ManagedEnrollmentClient({
    apiOrigin: meta === null ? 'https://app.whalerein.com' : 'https://app.whalerein.com',
    metadata: meta,
    hostId: HOST_ID,
    signingPrivateKey,
    signingPublicKey,
    noisePublicKey,
    credentialStore: store,
    journal,
    fetch: fetchImpl,
    now,
    sleep,
    allowLoopback: true,
    verifyAuthorization,
    expectedIssuer,
    locale: 'zh-CN',
  })
  return { client, store, journal, root, credentials }
}

function authorization(overrides = {}) {
  return {
    authVersion: AUTHORIZATION_PREFIX,
    authorizationId: 'auth-1',
    issuer: 'https://issuer.example',
    audience: ACCESS_ORIGIN,
    purpose: 'host_enrollment',
    workspaceId: 'ws-1',
    regionId: 'r1',
    operationId: 'op-1',
    enrollmentRevision: '1',
    precondition: '',
    expectedRevision: '',
    hostId: HOST_ID,
    hostPublicKeys: [b64(signingPublicKey), b64(noisePublicKey)],
    deviceId: '',
    devicePublicKeys: [],
    issuedAtUnixMs: NOW - 1000,
    expiresAtUnixMs: NOW + 60_000,
    nonce: b64(Buffer.alloc(16, 3)),
    keyId: 'key-1',
    signature: b64(Buffer.alloc(64, 4)),
    ...overrides,
  }
}

function route(overrides = {}) {
  return { region: 'r1', accessOrigin: 'https://access.example', expiresAt: NOW + 60_000, ...overrides }
}

function createdBody(overrides = {}) {
  return {
    id: 'request-1',
    state: 'pending',
    approveUrl: 'https://app.whalerein.com/approve/request-1',
    pollToken: 'poll-token-1',
    expiresAt: new Date(NOW + 300_000).toISOString(),
    pollAfterMs: 2000,
    service: 'whalerein-cloud',
    environment: 'production',
    fingerprint: 'FP',
    displayFingerprint: 'FP-DISPLAY',
    ...overrides,
  }
}

test('canonical intent and Host ownership proof texts match the frozen contract', () => {
  assert.equal(
    canonicalIntentProof({
      intentId: 'i',
      hostId: 'h',
      signingKey: 's',
      noiseKey: 'n',
      service: 'svc',
      environment: 'production',
      displayName: 'My Mac',
      locale: 'en',
      issuedAt: '2026-01-01T00:00:00Z',
      nonce: 'abc',
    }),
    `${INTENT_PROOF_PREFIX}\ni\nh\ns\nn\nsvc\nproduction\nMy Mac\nen\n2026-01-01T00:00:00Z\nabc`,
  )
  assert.equal(
    hostProofText({ purpose: 'enrollment_status', hostId: 'h', subjectId: 'op', issuedAt: 5, nonce: 'n' }),
    `${HOST_PROOF_PREFIX}\n|enrollment_status\n|h\n|op\n|5\n|n`,
  )
  assert.equal(
    enrollmentProofInput({ authorization: { authorizationId: 'a', nonce: b64(Buffer.alloc(8, 2)), hostId: 'h' }, operationId: 'op', hostSigningPublicKey: Buffer.alloc(32, 1) }),
    `${ENROLLMENT_PROOF_PREFIX}|a|${b64(Buffer.alloc(8, 2))}|h|${b64(Buffer.alloc(32, 1))}|op`,
  )
})

test('canonical authorization hashing is key-order independent', () => {
  const a = authorization({ hostPublicKeys: [b64(noisePublicKey), b64(signingPublicKey)] })
  const b = authorization({ hostPublicKeys: [b64(signingPublicKey), b64(noisePublicKey)] })
  assert.equal(canonicalAuthorizationInput(a), canonicalAuthorizationInput(b))
  assert.notEqual(canonicalAuthorizationInput(a), canonicalAuthorizationInput({ ...a, authorizationId: 'other' }))
  assert.match(canonicalAuthorizationInput(a), new RegExp(`^${AUTHORIZATION_PREFIX}\\n\\|auth-1\\n\\|https://issuer.example`))
})

test('Host ownership proof header is base64url JSON signed with the approved key', () => {
  const header = createHostProof({ signingPrivateKey, hostId: HOST_ID, purpose: 'enrollment_status', subjectId: 'op-1', issuedAt: NOW, nonce: 'nonce-1' })
  const decoded = JSON.parse(Buffer.from(header, 'base64url').toString('utf8'))
  assert.equal(decoded.hostId, HOST_ID)
  assert.equal(decoded.issuedAt, NOW)
  assert.equal(decoded.nonce, 'nonce-1')
  const text = hostProofText({ purpose: 'enrollment_status', hostId: HOST_ID, subjectId: 'op-1', issuedAt: NOW, nonce: 'nonce-1' })
  assert.equal(ed25519.verify(Buffer.from(decoded.signature, 'base64url'), new TextEncoder().encode(text), signingPublicKey), true)
  assert.throws(() => createHostProof({ signingPrivateKey: new Uint8Array(2), hostId: HOST_ID, purpose: 'enrollment_status' }), (error) => error.code === 'internal')
  assert.throws(() => createHostProof({ signingPrivateKey, hostId: '', purpose: 'enrollment_status' }), (error) => error.code === 'internal')
  assert.throws(() => createHostProof({ signingPrivateKey, hostId: HOST_ID, purpose: 'other' }), (error) => error.code === 'internal')
})

test('create intent persists the bound intent before the request and signs it', async () => {
  const requests = []
  const fixture = clientFixture({
    fetch: async (url, options) => {
      requests.push({ url, options })
      return response(201, createdBody())
    },
  })
  try {
    const intent = await fixture.client.createIntent()
    assert.equal(intent.id, 'request-1')
    assert.equal(intent.approveUrl, 'https://app.whalerein.com/approve/request-1?locale=zh-CN')
    assert.equal(intent.fingerprint, 'FP')
    assert.equal(await fixture.store.get(CREDENTIAL_PURPOSES.pollCredential), 'poll-token-1')
    const persisted = JSON.parse(readFileSync(join(fixture.root, 'enrollment-intent.json'), 'utf8'))
    assert.equal(persisted.intentId, intent.intentId)
    assert.equal(persisted.state, 'awaiting_approval')
    assert.equal(requests.length, 1)
    const body = JSON.parse(requests[0].options.body)
    assert.equal(body.intentId, intent.intentId)
    assert.equal(body.hostId, HOST_ID)
    assert.equal(body.signingKey, b64(signingPublicKey))
    assert.equal(body.noiseKey, b64(noisePublicKey))
    assert.equal(body.service, 'whalerein-cloud')
    const canonical = canonicalIntentProof({ ...body, issuedAt: body.proof.issuedAt, nonce: body.proof.nonce })
    assert.equal(ed25519.verify(Buffer.from(body.proof.signature, 'base64url'), new TextEncoder().encode(canonical), signingPublicKey), true)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('create reply loss replays the same intent with a fresh proof and rotated token', async () => {
  let attempt = 0
  const bodies = []
  const fixture = clientFixture({
    fetch: async (url, options) => {
      attempt += 1
      bodies.push(JSON.parse(options.body))
      if (attempt === 1) throw new TypeError('socket closed')
      return response(200, createdBody({ pollToken: 'poll-token-2' }))
    },
  })
  try {
    await assert.rejects(fixture.client.createIntent(), (error) => error.code === 'unavailable' && error.recoverable === true)
    assert.equal(JSON.parse(readFileSync(join(fixture.root, 'enrollment-intent.json'), 'utf8')).state, 'create_uncertain')
    const replayed = await fixture.client.resume()
    assert.equal(replayed.intentId, bodies[0].intentId)
    assert.equal(bodies[1].intentId, bodies[0].intentId)
    assert.notEqual(bodies[1].proof.nonce, bodies[0].proof.nonce)
    assert.equal(await fixture.store.get(CREDENTIAL_PURPOSES.pollCredential), 'poll-token-2')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('create response validation rejects environment, approval origin and incomplete replies', async () => {
  const cases = [
    [response(201, createdBody({ service: 'other' })), 'forbidden'],
    [response(201, createdBody({ pollToken: '' })), 'protocol_version_incompatible'],
    [response(201, createdBody({ approveUrl: 'https://evil.example/approve/1' })), 'forbidden'],
    [response(201, createdBody({ approveUrl: 'not a url' })), 'protocol_version_incompatible'],
    [response(201, createdBody({ expiresAt: 'soon' })), 'protocol_version_incompatible'],
    [response(400, { code: 'invalid_proof' }), 'forbidden'],
    [response(409, { code: 'intent_conflict' }), 'conflict'],
    [response(409, { code: 'approval_expired' }), 'expired'],
    [response(429, { code: 'rate_limited' }), 'rate_limited'],
    [response(503, { code: 'enrollment_unavailable' }), 'unavailable'],
    [response(500, {}), 'unavailable'],
  ]
  for (const [reply, code] of cases) {
    const fixture = clientFixture({ fetch: async () => reply })
    try {
      await assert.rejects(fixture.client.createIntent(), (error) => error.code === code, code)
    } finally {
      rmSync(fixture.root, { recursive: true, force: true })
    }
  }
})

test('enrollment sends one canonical protojson authorization spelling', async () => {
  let body = null
  const fixture = clientFixture({
    fetch: async (url, options) => {
      if (url.endsWith('/v1/enrollments/hosts')) {
        body = JSON.parse(options.body)
        return response(200, {
          status: {
            hostId: HOST_ID,
            workspaceId: 'ws-1',
            enrollmentRevision: '1',
            state: 'enrollment_pending',
            operationId: 'op-1',
            hostSigningPublicKey: b64(signingPublicKey),
            hostNoisePublicKey: b64(noisePublicKey),
          },
          credential: { hostId: HOST_ID, mqttUrl: 'mqtt://127.0.0.1:1883', username: `host:${HOST_ID}`, password: 'secret' },
        })
      }
      return response(500, {})
    },
  })
  try {
    await fixture.client.enroll({ authorization: { ...authorization(), auth_version: 'duplicate', authorization_id: 'duplicate' }, route: route(), operationId: 'op-1' })
    const keys = Object.keys(body.authorization)
    assert.ok(keys.includes('authorizationId'))
    assert.ok(!keys.some((key) => key.includes('_')), `snake_case keys must not be sent: ${keys.join(',')}`)
    const duplicates = keys.filter((key) => key.endsWith('Id') && keys.includes(key.replace(/Id$/, '_id')))
    assert.deepEqual(duplicates, [])
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('service metadata fetch validates origin, redirect, status and JSON', async () => {
  const document = {
    service: 'whalerein-cloud',
    environment: 'production',
    protocol: { version: 1, capabilities: ['host-enrollment-v1'] },
    approval: { origin: 'https://app.whalerein.com', path: '/approve/{id}' },
    enrollment: { createPath: '/api/enrollment-requests', pollAfterMs: 2000 },
  }
  const ok = await fetchServiceMetadata({ apiOrigin: 'https://app.whalerein.com', fetch: async () => response(200, document) })
  assert.equal(ok.service, 'whalerein-cloud')
  const failures = [
    [async () => { throw new TypeError('offline') }, 'unavailable'],
    [async () => ({ ...response(200, document), redirected: true }), 'forbidden'],
    [async () => ({ ...response(200, document), url: 'https://evil.example/api' }), 'forbidden'],
    [async () => response(503, {}), 'unavailable'],
    [async () => response(404, {}), 'protocol_version_incompatible'],
    [async () => ({ status: 200, json: async () => { throw new Error('bad') } }), 'protocol_version_incompatible'],
  ]
  for (const [fetchImpl, code] of failures) {
    await assert.rejects(fetchServiceMetadata({ apiOrigin: 'https://app.whalerein.com', fetch: fetchImpl }), (error) => error.code === code)
  }
  await assert.rejects(fetchServiceMetadata({ apiOrigin: 'https://app.whalerein.com' }), (error) => error.code === 'unavailable')
})

test('poll honors server intervals, stops on cancellation and fences stale generations', async () => {
  const delays = []
  let calls = 0
  const fixture = clientFixture({
    sleep: async (ms) => {
      delays.push(ms)
    },
    fetch: async (url, options) => {
      if (options.method === 'POST') return response(201, createdBody())
      calls += 1
      if (calls === 1) return response(200, { id: 'request-1', state: 'pending' }, { 'retry-after': '3' })
      return response(200, { id: 'request-1', state: 'approved', authorization: authorization(), route: route() })
    },
  })
  try {
    await fixture.client.createIntent()
    const approved = await fixture.client.poll({ isActive: () => true })
    assert.equal(approved.operationId, 'op-1')
    assert.ok(delays.includes(3000))
    await assert.rejects(
      fixture.client.poll({ isActive: () => false }),
      (error) => error.code === 'cancelled',
    )
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('poll stops on denial, expiry, wrong credential, missing request and deadline', async () => {
  const cases = [
    [async () => response(200, { id: 'request-1', state: 'denied' }), 'denied'],
    [async () => response(200, { id: 'request-1', state: 'expired' }), 'expired'],
    [async () => response(200, { id: 'request-1', state: 'unknown' }), 'protocol_version_incompatible'],
    [async () => response(200, { id: 'other', state: 'pending' }), 'forbidden'],
    [async () => response(200, { hostId: 'other', state: 'pending' }), 'forbidden'],
    [async () => response(401, {}), 'unauthorized'],
    [async () => response(404, {}), 'not_found'],
    [async () => response(429, {}, { 'retry-after': '0' }), 'expired'],
  ]
  for (const [pollReply, code] of cases) {
    let call = 0
    const fixture = clientFixture({
      sleep: async () => {},
      now: () => (call++ > 2 && code === 'expired' ? NOW + DEFAULT_DEADLINE_MS + 1 : NOW),
      fetch: async (url, options) => (options.method === 'POST' ? response(201, createdBody()) : pollReply()),
    })
    try {
      await fixture.client.createIntent()
      await assert.rejects(fixture.client.poll(), (error) => error.code === code, code)
    } finally {
      rmSync(fixture.root, { recursive: true, force: true })
    }
  }
})

test('poll surfaces a missing poll credential and resumes a lost create', async () => {
  const fixture = clientFixture({ fetch: async () => response(201, createdBody()) })
  try {
    await fixture.client.createIntent()
    await fixture.store.remove(CREDENTIAL_PURPOSES.pollCredential)
    await assert.rejects(fixture.client.poll(), (error) => error.code === 'unauthorized')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('approval validation rejects every route and binding mismatch before enrollment', () => {
  const fixture = clientFixture({ fetch: async () => response(500, {}) })
  try {
    const validate = (payload) => fixture.client.validateApproval(payload)
    assert.throws(() => validate(null), (error) => error.code === 'protocol_version_incompatible')
    assert.throws(() => validate({ route: route() }), (error) => error.code === 'forbidden')
    assert.throws(() => validate({ authorization: authorization() }), (error) => error.code === 'forbidden')
    const matrix = [
      [{ authVersion: 'v2' }, 'protocol_version_incompatible'],
      [{ purpose: 'device_pairing' }, 'forbidden'],
      [{ issuer: 'https://evil' }, 'forbidden', { expectedIssuer: 'https://issuer.example' }],
      [{ audience: 'other' }, 'forbidden'],
      [{ hostId: 'other' }, 'forbidden'],
      [{ operationId: '' }, 'forbidden'],
      [{ enrollmentRevision: '' }, 'forbidden'],
      [{ issuedAtUnixMs: NOW + 10 * 60 * 1000 }, 'forbidden'],
      [{ expiresAtUnixMs: NOW - 1 }, 'expired'],
      [{ hostPublicKeys: [b64(signingPublicKey)] }, 'forbidden'],
      [{ nonce: '' }, 'forbidden'],
      [{ signature: b64(Buffer.alloc(2)) }, 'forbidden'],
    ]
    for (const [override, code, options] of matrix) {
      const client = options
        ? clientFixture({ fetch: async () => response(500, {}), expectedIssuer: options.expectedIssuer }).client
        : fixture.client
      assert.throws(() => client.validateApproval({ authorization: authorization(override), route: route() }), (error) => error.code === code, code)
    }
    const routeMatrix = [
      [{ region: '' }, 'forbidden'],
      [{ region: 'other' }, 'forbidden'],
      [{ expiresAt: NOW - 1 }, 'expired'],
      [{ accessOrigin: 'http://access.example' }, 'forbidden'],
      [{ accessOrigin: 'https://user@access.example' }, 'forbidden'],
      [{ accessOrigin: 'https://access.example/path' }, 'forbidden'],
    ]
    for (const [override, code] of routeMatrix) {
      assert.throws(() => fixture.client.validateApproval({ authorization: authorization(), route: route(override) }), (error) => error.code === code, code)
    }
    // A route whose raw origin matches the authorization is validated on its own.
    const routeOriginMatrix = [
      ['not a url', 'forbidden'],
      ['http://access.example', 'forbidden'],
      ['https://user@access.example', 'forbidden'],
    ]
    for (const [accessOrigin, code] of routeOriginMatrix) {
      assert.throws(
        () => fixture.client.validateApproval({ authorization: authorization({ audience: accessOrigin }), route: route({ accessOrigin }) }),
        (error) => error.code === code,
        accessOrigin,
      )
    }
    // Loopback development may use HTTP, but only for an explicit loopback host.
    const loopback = clientFixture({ fetch: async () => response(500, {}) }).client
    const loopbackAuth = authorization({ audience: 'http://127.0.0.1:8081' })
    assert.equal(loopback.validateApproval({ authorization: loopbackAuth, route: route({ accessOrigin: 'http://127.0.0.1:8081' }) }).route.accessOrigin, 'http://127.0.0.1:8081')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('an injected issuer verifier must accept the canonical authorization bytes', () => {
  let seen = null
  const fixture = clientFixture({
    fetch: async () => response(500, {}),
    verifyAuthorization: (auth, canonical) => {
      seen = canonical
      return auth.authorizationId === 'auth-1'
    },
  })
  try {
    const approved = fixture.client.validateApproval({ authorization: authorization(), route: route() })
    assert.equal(approved.operationId, 'op-1')
    assert.equal(seen, canonicalAuthorizationInput(authorization()))
    const rejecting = clientFixture({ fetch: async () => response(500, {}), verifyAuthorization: () => false }).client
    assert.throws(() => rejecting.validateApproval({ authorization: authorization(), route: route() }), (error) => error.code === 'forbidden')
    const throwing = clientFixture({ fetch: async () => response(500, {}), verifyAuthorization: () => { throw new Error('bad') } }).client
    assert.throws(() => throwing.validateApproval({ authorization: authorization(), route: route() }), (error) => error.code === 'forbidden')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('enrollment signs the Access proof, persists the credential before connecting and clears poll state', async () => {
  const requests = []
  const fixture = clientFixture({
    fetch: async (url, options) => {
      if (options.method === 'POST' && url.endsWith('/api/enrollment-requests')) return response(201, createdBody())
      requests.push({ url, options })
      return response(200, {
        status: {
          hostId: HOST_ID,
          enrollmentRevision: '1',
          state: 'enrollment_pending',
          hostSigningPublicKey: b64(signingPublicKey),
          hostNoisePublicKey: b64(noisePublicKey),
        },
        credential: { hostId: HOST_ID, mqttUrl: 'mqtts://broker.example:8883', username: `host:${HOST_ID}`, password: 'broker-password' },
      })
    },
  })
  try {
    await fixture.client.createIntent()
    const enrollment = await fixture.client.enroll({ authorization: authorization(), route: route(), operationId: 'op-1' })
    assert.equal(enrollment.credential.password, 'broker-password')
    const stored = await fixture.store.getJson(CREDENTIAL_PURPOSES.transportCredential)
    assert.equal(stored.password, 'broker-password')
    assert.equal(stored.accessOrigin, 'https://access.example')
    assert.equal(await fixture.store.get(CREDENTIAL_PURPOSES.pollCredential), null)
    assert.equal(requests.length, 1)
    assert.equal(requests[0].url, 'https://access.example/v1/enrollments/hosts')
    const body = JSON.parse(requests[0].options.body)
    assert.equal(body.operationId, 'op-1')
    assert.equal(body.hostId, HOST_ID)
    assert.equal(body.hostSigningPublicKey, Buffer.from(signingPublicKey).toString('base64'))
    assert.equal(body.hostNoisePublicKey, Buffer.from(noisePublicKey).toString('base64'))
    const proofText = enrollmentProofInput({ authorization: authorization(), operationId: 'op-1', hostSigningPublicKey: signingPublicKey })
    assert.equal(ed25519.verify(Buffer.from(body.proofSignature, 'base64'), new TextEncoder().encode(proofText), signingPublicKey), true)
    assert.equal(JSON.parse(readFileSync(join(fixture.root, 'enrollment-intent.json'), 'utf8')).state, 'enrolled')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('enrollment reply loss reads proof-scoped status and never duplicates enrollment', async () => {
  const requests = []
  const fixture = clientFixture({
    fetch: async (url, options) => {
      requests.push({ url, options })
      if (options.method === 'POST' && url.endsWith('/api/enrollment-requests')) return response(201, createdBody())
      if (url.includes('/enrollment')) {
        assert.equal(options.method, 'GET')
        return response(200, { hostId: HOST_ID, state: 'enrollment_pending', enrollmentRevision: '1' })
      }
      throw new TypeError('reply lost')
    },
  })
  try {
    await fixture.client.createIntent()
    await assert.rejects(
      fixture.client.enroll({ authorization: authorization(), route: route(), operationId: 'op-1' }),
      (error) => error.code === 'recovery_required' && error.committed === true,
    )
    const statusRequest = requests.find((request) => request.url.includes('/v1/hosts/'))
    assert.ok(statusRequest)
    const header = JSON.parse(Buffer.from(statusRequest.options.headers['X-Whalerein-Host-Proof'], 'base64url').toString('utf8'))
    assert.equal(header.hostId, HOST_ID)
    const text = hostProofText({ purpose: 'enrollment_status', hostId: HOST_ID, subjectId: 'op-1', issuedAt: header.issuedAt, nonce: header.nonce })
    assert.equal(ed25519.verify(Buffer.from(header.signature, 'base64url'), new TextEncoder().encode(text), signingPublicKey), true)
    assert.equal(JSON.parse(readFileSync(join(fixture.root, 'enrollment-intent.json'), 'utf8')).state, 'uncertain')
    // Only the original commit attempt was sent; recovery never re-enrolls.
    assert.equal(requests.filter((request) => request.url.endsWith('/v1/enrollments/hosts')).length, 1)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('enrollment credential persistence failure stays explicitly unresolved', async () => {
  const credentials = new MemoryHostCredentials()
  const store = new ProfileCredentialStore({ credentials, profileId: PROFILE_ID })
  let failJson = false
  const wrapped = {
    get: (purpose) => store.get(purpose),
    set: (purpose, value) => store.set(purpose, value),
    remove: (purpose) => store.remove(purpose),
    getJson: (purpose) => store.getJson(purpose),
    setJson: async (purpose, value) => {
      if (failJson) throw new Error('disk full')
      return store.setJson(purpose, value)
    },
  }
  const root = mkdtempSync(join(tmpdir(), 'whalerein-enrollment-'))
  try {
    const client = new ManagedEnrollmentClient({
      apiOrigin: 'https://app.whalerein.com',
      metadata: metadata(),
      hostId: HOST_ID,
      signingPrivateKey,
      signingPublicKey,
      noisePublicKey,
      credentialStore: wrapped,
      journal: new IntentJournal({ stateDir: root }),
      fetch: async () => response(200, {
        status: { hostId: HOST_ID, enrollmentRevision: '1', hostSigningPublicKey: b64(signingPublicKey), hostNoisePublicKey: b64(noisePublicKey) },
        credential: { hostId: HOST_ID, mqttUrl: 'mqtts://broker.example', username: `host:${HOST_ID}`, password: 'secret' },
      }),
      now: () => NOW,
    })
    failJson = true
    await assert.rejects(client.enroll({ authorization: authorization(), route: route(), operationId: 'op-1' }), (error) => error.code === 'recovery_required' && error.committed === true)
    assert.equal(await store.getJson(CREDENTIAL_PURPOSES.transportCredential), null)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('enrollment rejects conflicts, refusals, mismatched responses and unsafe credentials', async () => {
  const cases = [
    [response(409, {}), 'conflict'],
    [response(401, {}), 'forbidden'],
    [response(404, {}), 'not_found'],
    [response(500, {}), 'unavailable'],
    [response(200, { status: { hostId: 'other' } }), 'forbidden'],
    [response(200, { status: { hostId: HOST_ID, hostSigningPublicKey: b64(Buffer.alloc(32, 9)) } }), 'forbidden'],
    [response(200, { status: { hostId: HOST_ID, hostSigningPublicKey: b64(signingPublicKey), hostNoisePublicKey: b64(noisePublicKey) }, credential: undefined }), 'recovery_required'],
    [response(200, { status: { hostId: HOST_ID }, credential: { hostId: HOST_ID, mqttUrl: 'mqtt://broker.example', username: `host:${HOST_ID}`, password: 'x' } }), 'forbidden'],
    [response(200, { status: { hostId: HOST_ID }, credential: { hostId: HOST_ID, mqttUrl: 'mqtts://broker.example', username: 'other', password: 'x' } }), 'forbidden'],
    [response(200, { status: { hostId: HOST_ID }, credential: { hostId: HOST_ID, mqttUrl: 'mqtts://user:pass@broker.example', username: `host:${HOST_ID}`, password: 'x' } }), 'forbidden'],
    [response(200, { status: { hostId: HOST_ID }, credential: { hostId: HOST_ID, mqttUrl: 'nope', username: `host:${HOST_ID}`, password: 'x' } }), 'forbidden'],
    [response(200, { status: { hostId: HOST_ID }, credential: { hostId: HOST_ID, mqttUrl: 'mqtts://broker.example', username: `host:${HOST_ID}`, password: '' } }), 'forbidden'],
  ]
  for (const [reply, code] of cases) {
    const fixture = clientFixture({ fetch: async () => reply })
    try {
      await assert.rejects(fixture.client.enroll({ authorization: authorization(), route: route(), operationId: 'op-1' }), (error) => error.code === code, code)
    } finally {
      rmSync(fixture.root, { recursive: true, force: true })
    }
  }
})

test('proof-scoped status reads and credential replacement use Host proof and persist once', async () => {
  const requests = []
  const fixture = clientFixture({
    fetch: async (url, options) => {
      requests.push({ url, options })
      if (url.includes('/credential')) {
        return response(200, {
          state: 'enrollment_ready',
          revision: '2',
          credential: { hostId: HOST_ID, mqttUrl: 'mqtts://broker.example', username: `host:${HOST_ID}`, password: 'replacement' },
        })
      }
      return response(200, { hostId: HOST_ID, state: 'enrollment_ready', enrollmentRevision: '2', operationId: 'op-1' })
    },
  })
  try {
    const status = await fixture.client.readStatus({ accessOrigin: 'https://access.example', operationId: 'op-1', nonce: 'n1' })
    assert.equal(status.state, 'enrollment_ready')
    assert.equal(status.revision, '2')
    const replacement = await fixture.client.replaceCredential({
      accessOrigin: 'https://access.example',
      authorization: authorization({ purpose: 'host_credential_recovery', authorizationId: 'recovery-1', operationId: 'op-2', hostPublicKeys: [b64(signingPublicKey), b64(noisePublicKey)], audience: ACCESS_ORIGIN }),
      nonce: 'n2',
    })
    assert.equal(replacement.password, 'replacement')
    const stored = await fixture.store.getJson(CREDENTIAL_PURPOSES.transportCredential)
    assert.equal(stored.password, 'replacement')
    const replacementRequest = requests.find((request) => request.url.includes('/credential'))
    const header = JSON.parse(Buffer.from(replacementRequest.options.headers['X-Whalerein-Host-Proof'], 'base64url').toString('utf8'))
    const text = hostProofText({ purpose: 'credential_replacement', hostId: HOST_ID, subjectId: 'recovery-1', issuedAt: header.issuedAt, nonce: 'n2' })
    assert.equal(ed25519.verify(Buffer.from(header.signature, 'base64url'), new TextEncoder().encode(text), signingPublicKey), true)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('proof-scoped status and replacement failures are typed', async () => {
  const fixture = clientFixture({ fetch: async () => response(500, {}) })
  try {
    await assert.rejects(fixture.client.readStatus({ accessOrigin: 'https://access.example' }), (error) => error.code === 'unavailable')
    for (const [reply, code] of [[response(401, {}), 'unauthorized'], [response(404, {}), 'not_found'], [response(500, {}), 'unavailable'], [response(200, { hostId: 'other' }), 'forbidden'], [response(200, { hostId: HOST_ID, hostSigningPublicKey: b64(Buffer.alloc(32, 1)) }), 'forbidden']]) {
      const client = clientFixture({ fetch: async () => reply }).client
      await assert.rejects(client.readStatus({ accessOrigin: 'https://access.example' }), (error) => error.code === code, code)
    }
    const recovery = authorization({ purpose: 'host_credential_recovery', authorizationId: 'recovery-1' })
    await assert.rejects(fixture.client.replaceCredential({ accessOrigin: 'https://access.example', authorization: authorization() }), (error) => error.code === 'forbidden')
    await assert.rejects(fixture.client.replaceCredential({ accessOrigin: 'https://access.example', authorization: { ...recovery, hostId: 'other' } }), (error) => error.code === 'forbidden')
    await assert.rejects(fixture.client.replaceCredential({ accessOrigin: 'https://access.example', authorization: { ...recovery, expiresAtUnixMs: NOW - 1 } }), (error) => error.code === 'expired')
    for (const [reply, code] of [[response(401, {}), 'forbidden'], [response(409, {}), 'conflict'], [response(404, {}), 'not_found'], [response(500, {}), 'unavailable'], [response(200, {}), 'forbidden'], [response(200, { credential: { hostId: HOST_ID, mqttUrl: 'mqtts://broker', username: `host:${HOST_ID}`, password: 'x' } }), 'missing']]) {
      const client = clientFixture({ fetch: async () => reply }).client
      if (code === 'missing') {
        assert.equal((await client.replaceCredential({ accessOrigin: 'https://access.example', authorization: recovery })).password, 'x')
      } else {
        await assert.rejects(client.replaceCredential({ accessOrigin: 'https://access.example', authorization: recovery }), (error) => error.code === code, code)
      }
    }
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('intent journal rejects corrupt content and surfaces write failures', () => {
  const root = mkdtempSync(join(tmpdir(), 'whalerein-intent-'))
  try {
    const journal = new IntentJournal({ stateDir: root })
    assert.equal(journal.read(), null)
    writeFileSync(join(root, 'enrollment-intent.json'), '{bad')
    assert.throws(() => journal.read(), (error) => error.code === 'corrupt_intent')
    writeFileSync(join(root, 'enrollment-intent.json'), JSON.stringify({ schema: 'other', intentId: 'x' }))
    assert.throws(() => journal.read(), (error) => error.code === 'corrupt_intent')
    assert.throws(() => journal.write({ schema: 'other', intentId: 'x' }), (error) => error.code === 'invalid_argument')
    const failing = new IntentJournal({ stateDir: root, write: () => { throw new Error('disk') } })
    assert.throws(() => failing.write({ schema: 'whalerein-host/v1/enrollment-intent', intentId: 'x' }), (error) => error.code === 'unavailable')
    journal.clear()
    journal.clear()
    assert.throws(() => new IntentJournal({}), (error) => error.code === 'invalid_argument')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('enrollment constructor rejects missing keys, stores, fetch and locale', () => {
  const base = {
    apiOrigin: 'https://app.whalerein.com',
    metadata: metadata(),
    hostId: HOST_ID,
    signingPrivateKey,
    signingPublicKey,
    noisePublicKey,
    credentialStore: new ProfileCredentialStore({ credentials: new MemoryHostCredentials(), profileId: PROFILE_ID }),
    journal: new IntentJournal({ stateDir: mkdtempSync(join(tmpdir(), 'whalerein-enroll-')) }),
    fetch: async () => response(500, {}),
  }
  const cases = [
    { ...base, metadata: null },
    { ...base, hostId: 'bad host' },
    { ...base, signingPrivateKey: new Uint8Array(8) },
    { ...base, signingPublicKey: new Uint8Array(8) },
    { ...base, noisePublicKey: new Uint8Array(8) },
    { ...base, credentialStore: {} },
    { ...base, fetch: null },
    { ...base, locale: 'fr' },
  ]
  for (const options of cases) {
    assert.throws(() => new ManagedEnrollmentClient(options), (error) => error.code === 'invalid_argument')
  }
  for (const stateDir of [base.journal]) rmSync(stateDir.path.replace('/enrollment-intent.json', ''), { recursive: true, force: true })
})

test('an existing pending intent is returned without a second request and retry clears it', async () => {
  let requests = 0
  const fixture = clientFixture({ fetch: async () => { requests += 1; return response(201, createdBody()) } })
  try {
    const first = await fixture.client.createIntent()
    const second = await fixture.client.createIntent()
    assert.equal(first.intentId, second.intentId)
    assert.equal(requests, 1)
    await fixture.client.createIntent({ retry: true })
    assert.equal(requests, 2)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('poll retries transient transport and server errors with bounded sleep', async () => {
  const delays = []
  let call = 0
  const fixture = clientFixture({
    sleep: async (ms) => { delays.push(ms) },
    fetch: async (url, options) => {
      if (options.method === 'POST') return response(201, createdBody())
      call += 1
      if (call === 1) throw new TypeError('offline')
      if (call === 2) return response(429, {}, { 'retry-after': new Date(Date.now() + 2000).toUTCString() })
      if (call === 3) return response(500, {})
      return response(200, { id: 'request-1', state: 'denied' })
    },
  })
  try {
    await fixture.client.createIntent()
    await assert.rejects(fixture.client.poll(), (error) => error.code === 'denied')
    assert.equal(delays.length >= 3, true)
    assert.equal(call, 4)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('poll aborts immediately when the caller signal is already aborted', async () => {
  const fixture = clientFixture({ fetch: async () => response(201, createdBody()) })
  try {
    await fixture.client.createIntent()
    const controller = new AbortController()
    controller.abort()
    await assert.rejects(fixture.client.poll({ signal: controller.signal }), (error) => error.code === 'cancelled')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('approval URL validation handles fallback, insecure origins and locale replacement', async () => {
  const loose = clientFixture({
    fetch: async (url, options) => {
      if (options.method === 'POST') {
        return response(201, createdBody({ approveUrl: '' }))
      }
      return response(200, { id: 'request-1', state: 'pending' })
    },
  })
  try {
    const intent = await loose.client.createIntent()
    assert.equal(intent.approveUrl, 'https://app.whalerein.com/approve/request-1?locale=zh-CN')
  } finally {
    rmSync(loose.root, { recursive: true, force: true })
  }
  const insecureMetadata = validateServiceMetadata({
    service: 'local-service',
    environment: 'custom',
    protocol: { version: 1, capabilities: ['host-enrollment-v1'] },
    approval: { origin: 'http://127.0.0.1:9999', path: '/approve/{id}' },
    enrollment: { createPath: '/api/enrollment-requests' },
  }, { allowLoopback: true })
  const insecure = clientFixture({ metadata: insecureMetadata, fetch: async () => response(201, createdBody({ service: 'local-service', environment: 'custom', approveUrl: 'http://127.0.0.1:9999/approve/request-1' })) })
  try {
    const intent = await insecure.client.createIntent()
    assert.match(intent.approveUrl, /^http:\/\/127\.0\.0\.1:9999\/approve\/request-1\?locale=zh-CN$/)
  } finally {
    rmSync(insecure.root, { recursive: true, force: true })
  }
})

test('enrollment rejects missing arguments and non-canonical MQTT URLs', async () => {
  const fixture = clientFixture({ fetch: async () => response(200, {}) })
  try {
    await assert.rejects(fixture.client.enroll({ authorization: null, route: route(), operationId: 'op-1' }), (error) => error.code === 'invalid_argument')
    for (const mqttUrl of ['mqtts://broker.example/path', 'mqtts://broker.example?x=1', 'mqtts://broker.example#fragment']) {
      const client = clientFixture({ fetch: async () => response(200, {
        status: { hostId: HOST_ID },
        credential: { hostId: HOST_ID, mqttUrl, username: `host:${HOST_ID}`, password: 'x' },
      }) }).client
      await assert.rejects(client.enroll({ authorization: authorization(), route: route(), operationId: 'op-1' }), (error) => error.code === 'forbidden')
    }
    // Loopback plaintext MQTT is permitted only with explicit development opt-in.
    const loopback = clientFixture({ fetch: async () => response(200, {
      status: { hostId: HOST_ID, hostSigningPublicKey: b64(signingPublicKey), hostNoisePublicKey: b64(noisePublicKey) },
      credential: { hostId: HOST_ID, mqttUrl: 'mqtt://127.0.0.1:1883', username: `host:${HOST_ID}`, password: 'x' },
    }) }).client
    const enrolled = await loopback.enroll({ authorization: authorization(), route: route(), operationId: 'op-1' })
    assert.equal(enrolled.credential.mqttUrl, 'mqtt://127.0.0.1:1883')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('credential replacement and status reads reject transport failures and malformed data', async () => {
  const failing = clientFixture({ fetch: async () => { throw new TypeError('offline') } })
  try {
    await assert.rejects(
      failing.client.replaceCredential({ accessOrigin: 'https://access.example', authorization: authorization({ purpose: 'host_credential_recovery', authorizationId: 'r1' }) }),
      (error) => error.code === 'unavailable',
    )
    await assert.rejects(failing.client.readStatus({ accessOrigin: 'https://access.example' }), (error) => error.code === 'unavailable')
  } finally {
    rmSync(failing.root, { recursive: true, force: true })
  }
  const badStatus = clientFixture({ fetch: async () => response(200, { hostId: HOST_ID, hostSigningPublicKey: Buffer.from(signingPublicKey).toString('base64') }) })
  try {
    const status = await badStatus.client.readStatus({ accessOrigin: 'https://access.example' })
    assert.equal(status.state, '')
  } finally {
    rmSync(badStatus.root, { recursive: true, force: true })
  }
})

test('poll credential clearing tolerates adapters without delete and removal failures', async () => {
  const credentials = new MemoryHostCredentials()
  const store = new ProfileCredentialStore({ credentials, profileId: PROFILE_ID })
  const noDelete = {
    get: (purpose) => store.get(purpose),
    set: (purpose, value) => store.set(purpose, value),
    getJson: (purpose) => store.getJson(purpose),
    setJson: (purpose, value) => store.setJson(purpose, value),
  }
  const root = mkdtempSync(join(tmpdir(), 'whalerein-enrollment-'))
  try {
    const client = new ManagedEnrollmentClient({
      apiOrigin: 'https://app.whalerein.com',
      metadata: metadata(),
      hostId: HOST_ID,
      signingPrivateKey,
      signingPublicKey,
      noisePublicKey,
      credentialStore: noDelete,
      journal: new IntentJournal({ stateDir: root }),
      fetch: async () => response(200, {}),
      now: () => NOW,
    })
    await client.enroll({ authorization: authorization(), route: route(), operationId: 'op-1' }).catch(() => {})
    // Removal with an adapter that only supports set stores the empty marker.
    const throwing = {
      ...noDelete,
      remove: async () => { throw new Error('delete unavailable') },
    }
    const failingClient = new ManagedEnrollmentClient({
      apiOrigin: 'https://app.whalerein.com',
      metadata: metadata(),
      hostId: HOST_ID,
      signingPrivateKey,
      signingPublicKey,
      noisePublicKey,
      credentialStore: throwing,
      journal: new IntentJournal({ stateDir: root }),
      fetch: async (url, options) => (options.method === 'POST'
        ? response(201, createdBody())
        : response(200, { id: 'request-1', state: 'denied' })),
      now: () => NOW,
    })
    await failingClient.createIntent()
    await assert.rejects(failingClient.poll(), (error) => error.code === 'denied')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('intent journal read failure other than missing is actionable', () => {
  const root = mkdtempSync(join(tmpdir(), 'whalerein-intent-'))
  try {
    const journal = new IntentJournal({ stateDir: root })
    // A directory at the journal path is not readable as a file.
    mkdirSync(join(root, 'enrollment-intent.json'))
    assert.throws(() => journal.read(), (error) => error.code === 'corrupt_intent')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('recovery intent creation signs a Host proof and accepts both payload spellings', async () => {
  const requests = []
  const fixture = clientFixture({
    fetch: async (url, options) => {
      requests.push({ url, options })
      return response(201, {
        intent_id: 'intent-1',
        approval_url: 'https://app.whalerein.com/recover/intent-1',
        poll_token: 'poll-1',
        expires_at_unix_ms: NOW + 120_000,
      })
    },
  })
  try {
    const intent = await fixture.client.createRecoveryIntent({ apiOrigin: 'https://app.whalerein.com', nonce: 'n1' })
    assert.deepEqual(intent, {
      intentId: 'intent-1',
      approvalUrl: 'https://app.whalerein.com/recover/intent-1',
      pollToken: 'poll-1',
      expiresAtUnixMs: NOW + 120_000,
    })
    assert.equal(requests[0].url, 'https://app.whalerein.com/api/hosts/recovery-intents')
    assert.equal(requests[0].options.method, 'POST')
    assert.deepEqual(JSON.parse(requests[0].options.body), { host_id: HOST_ID })
    const header = JSON.parse(Buffer.from(requests[0].options.headers['X-Whalerein-Host-Proof'], 'base64url').toString('utf8'))
    const text = hostProofText({ purpose: 'recovery_intent', hostId: HOST_ID, subjectId: HOST_ID, issuedAt: header.issuedAt, nonce: 'n1' })
    assert.equal(ed25519.verify(Buffer.from(header.signature, 'base64url'), new TextEncoder().encode(text), signingPublicKey), true)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }

  const legacy = clientFixture({
    fetch: async () => response(200, {
      intentId: 'intent-2',
      approvalUrl: 'https://app.whalerein.com/recover/intent-2',
      pollToken: 'poll-2',
    }),
  })
  try {
    const intent = await legacy.client.createRecoveryIntent({ apiOrigin: 'https://app.whalerein.com' })
    assert.equal(intent.intentId, 'intent-2')
    assert.equal(intent.pollToken, 'poll-2')
    assert.equal(intent.expiresAtUnixMs, 0, 'a missing expiry defaults to zero for an explicit deadline fallback')
  } finally {
    rmSync(legacy.root, { recursive: true, force: true })
  }
})

test('recovery intent failures are typed and never retried on the wrong origin', async () => {
  let mode = 'throw'
  let reply = () => response(201, {})
  const fixture = clientFixture({
    fetch: async () => {
      if (mode === 'throw') throw new TypeError('offline')
      return reply()
    },
  })
  try {
    await assert.rejects(
      fixture.client.createRecoveryIntent({ apiOrigin: 'https://app.whalerein.com/path' }),
      (error) => error.code === 'invalid_argument',
      'a path on the service origin is rejected',
    )
    await assert.rejects(
      fixture.client.createRecoveryIntent({ apiOrigin: '' }),
      (error) => error.code === 'invalid_argument',
      'a missing service origin is rejected before any request',
    )
    await assert.rejects(fixture.client.createRecoveryIntent({ apiOrigin: 'https://app.whalerein.com' }), (error) => error.code === 'unavailable')
    mode = 'reply'
    await assert.rejects(fixture.client.createRecoveryIntent({ apiOrigin: 'https://app.whalerein.com' }), (error) => error.code === 'unavailable')
    const cases = [
      [response(401, {}), 'unauthorized'],
      [response(403, {}), 'unauthorized'],
      [response(409, {}), 'recovery_required'],
      [response(429, {}), 'unavailable'],
      [response(201, { intent_id: 'intent-1', approval_url: 'https://app.whalerein.com/recover/1' }), 'unavailable'],
      [response(201, { intent_id: 4, approval_url: 'https://app.whalerein.com/recover/1', poll_token: 'p' }), 'unavailable'],
      [response(200, { intentId: 'intent-1', approvalUrl: null, pollToken: 'p' }), 'unavailable'],
      [response(201, null), 'protocol_version_incompatible'],
    ]
    for (const [payload, code] of cases) {
      reply = () => payload
      await assert.rejects(
        fixture.client.createRecoveryIntent({ apiOrigin: 'https://app.whalerein.com' }),
        (error) => error.code === code,
        code,
      )
    }
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('recovery polling returns an approval with its route and backs off while pending', async () => {
  const delays = []
  let replies = [
    response(200, { state: 'pending' }),
    response(200, { state: 'pending' }),
    response(200, { state: 'approved', authorization: { authorizationId: 'recovery-1' } }),
  ]
  const fixture = clientFixture({
    sleep: async (ms) => { delays.push(ms) },
    fetch: async () => replies.shift(),
  })
  try {
    const approved = await fixture.client.pollRecovery({ apiOrigin: 'https://app.whalerein.com', intentId: 'intent-1', pollToken: 'poll-1' })
    assert.equal(approved.state, 'approved')
    assert.deepEqual(approved.authorization, { authorizationId: 'recovery-1' })
    assert.deepEqual(approved.route, { accessOrigin: 'https://app.whalerein.com' })
    assert.deepEqual(delays, [500, 1000], 'pending polls back off without a server interval')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }

  const delivered = clientFixture({
    fetch: async () => response(200, {
      state: 'delivered',
      authorization: { authorizationId: 'recovery-2' },
      route: { accessOrigin: 'https://access.example', region: 'r1' },
    }),
  })
  try {
    const result = await delivered.client.pollRecovery({ apiOrigin: 'https://app.whalerein.com', intentId: 'intent-2', pollToken: 'poll-2' })
    assert.equal(result.state, 'approved')
    assert.deepEqual(result.route, { accessOrigin: 'https://access.example', region: 'r1' })
  } finally {
    rmSync(delivered.root, { recursive: true, force: true })
  }
})

test('recovery polling stops on terminal states, refusal statuses and a missing authorization', async () => {
  const cases = [
    [response(200, { state: 'denied' }), 'denied'],
    [response(200, { state: 'failed' }), 'denied'],
    [response(200, { state: 'expired' }), 'expired'],
    [response(200, { state: 'approved' }), 'unavailable'],
    [response(401, {}), 'unauthorized'],
    [response(410, {}), 'expired'],
    [response(404, {}), 'not_found'],
    [response(500, {}), 'unavailable'],
  ]
  const fixture = clientFixture({ sleep: async () => {} })
  try {
    for (const [payload, code] of cases) {
      const client = clientFixture({ fetch: async () => payload }).client
      await assert.rejects(
        client.pollRecovery({ apiOrigin: 'https://app.whalerein.com', intentId: 'intent-1', pollToken: 'poll-1' }),
        (error) => error.code === code,
        code,
      )
    }
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('recovery polling retries a transport failure, honours cancellation and expires at its deadline', async () => {
  const delays = []
  let calls = 0
  const retrying = clientFixture({
    sleep: async (ms) => { delays.push(ms) },
    fetch: async () => {
      calls += 1
      if (calls === 1) throw new TypeError('offline')
      return response(200, { state: 'approved', authorization: { authorizationId: 'recovery-1' } })
    },
  })
  try {
    const approved = await retrying.client.pollRecovery({ apiOrigin: 'https://app.whalerein.com', intentId: 'intent-1', pollToken: 'poll-1' })
    assert.equal(approved.state, 'approved')
    assert.deepEqual(delays, [500])
  } finally {
    rmSync(retrying.root, { recursive: true, force: true })
  }

  const cancelled = clientFixture({ fetch: async () => response(200, { state: 'pending' }) })
  try {
    await assert.rejects(
      cancelled.client.pollRecovery({ apiOrigin: 'https://app.whalerein.com', intentId: 'intent-1', pollToken: 'poll-1', isActive: () => false }),
      (error) => error.code === 'cancelled',
    )
    await assert.rejects(
      cancelled.client.pollRecovery({ apiOrigin: 'https://app.whalerein.com', intentId: 'intent-1', pollToken: 'poll-1', deadlineMs: 0 }),
      (error) => error.code === 'expired',
    )
  } finally {
    rmSync(cancelled.root, { recursive: true, force: true })
  }
})

test('operation status normalizes optional credential fields and unknown outcomes', async () => {
  const replies = [
    response(200, { hostId: HOST_ID, outcome: 'pending', operationId: 'op-1' }),
    response(200, { hostId: HOST_ID, outcome: 'someday' }),
  ]
  const fixture = clientFixture({ fetch: async () => replies.shift() })
  try {
    const pending = await fixture.client.readOperation({ accessOrigin: ACCESS_ORIGIN, operationId: 'op-1' })
    assert.equal(pending.outcome, 'pending')
    assert.equal(pending.credentialIssued, false)
    assert.equal(pending.credentialGeneration, 0)
    assert.equal(pending.credentialOperationId, '')
    assert.equal(pending.reason, '')

    const unknown = await fixture.client.readOperation({ accessOrigin: ACCESS_ORIGIN, operationId: 'op-1' })
    assert.equal(unknown.outcome, 'unknown')
    assert.equal(unknown.operationId, 'op-1')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('operation status transport failures stay typed and never expose an invalid reply', async () => {
  const offline = clientFixture({ fetch: async () => { throw new TypeError('offline') } })
  try {
    await assert.rejects(
      offline.client.readOperation({ accessOrigin: ACCESS_ORIGIN, operationId: 'op-1' }),
      (error) => error.code === 'unavailable',
    )
  } finally {
    rmSync(offline.root, { recursive: true, force: true })
  }
  const redirected = clientFixture({
    fetch: async () => ({ status: 200, redirected: true, headers: { get: () => null }, json: async () => ({}) }),
  })
  try {
    await assert.rejects(
      redirected.client.readOperation({ accessOrigin: ACCESS_ORIGIN, operationId: 'op-1' }),
      (error) => error.code === 'forbidden',
      'a redirected proof request is refused instead of followed',
    )
  } finally {
    rmSync(redirected.root, { recursive: true, force: true })
  }
  const notFound = clientFixture({ fetch: async () => response(404, {}) })
  try {
    assert.deepEqual(
      await notFound.client.readOperation({ accessOrigin: ACCESS_ORIGIN, operationId: 'op-1' }),
      { outcome: 'absent', reason: 'not_admitted', operationId: 'op-1' },
    )
  } finally {
    rmSync(notFound.root, { recursive: true, force: true })
  }
})

test('removal status reads are proof-scoped and normalize every lifecycle state', async () => {
  const requests = []
  const fixture = clientFixture({
    fetch: async (url, options) => {
      requests.push({ url, options })
      return response(200, {
        state: 'removing',
        removal_operation_id: 'removal-1',
        target: { hostId: HOST_ID },
      })
    },
  })
  try {
    const observed = await fixture.client.readRemoval({ apiOrigin: 'https://app.whalerein.com', nonce: 'n1' })
    assert.deepEqual(observed, { state: 'removing', removalOperationId: 'removal-1', target: { hostId: HOST_ID } })
    assert.equal(requests[0].url, `https://app.whalerein.com/api/hosts/${HOST_ID}/removal`)
    const header = JSON.parse(Buffer.from(requests[0].options.headers['X-Whalerein-Host-Proof'], 'base64url').toString('utf8'))
    const text = hostProofText({ purpose: 'removal_status', hostId: HOST_ID, subjectId: HOST_ID, issuedAt: header.issuedAt, nonce: 'n1' })
    assert.equal(ed25519.verify(Buffer.from(header.signature, 'base64url'), new TextEncoder().encode(text), signingPublicKey), true)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }

  const unknown = clientFixture({ fetch: async () => response(200, { state: 'someday' }) })
  try {
    const observed = await unknown.client.readRemoval({ apiOrigin: 'https://app.whalerein.com' })
    assert.equal(observed.state, 'unavailable')
    assert.equal(observed.removalOperationId, '')
    assert.equal(observed.target, null)
  } finally {
    rmSync(unknown.root, { recursive: true, force: true })
  }

  const absent = clientFixture({ fetch: async () => response(404, {}) })
  try {
    assert.deepEqual(await absent.client.readRemoval({ apiOrigin: 'https://app.whalerein.com' }), { state: 'none' })
  } finally {
    rmSync(absent.root, { recursive: true, force: true })
  }
})

test('removal status failures are typed', async () => {
  const cases = [
    [response(401, {}), 'unauthorized'],
    [response(403, {}), 'unauthorized'],
    [response(500, {}), 'unavailable'],
  ]
  for (const [reply, code] of cases) {
    const fixture = clientFixture({ fetch: async () => reply })
    try {
      await assert.rejects(
        fixture.client.readRemoval({ apiOrigin: 'https://app.whalerein.com' }),
        (error) => error.code === code,
        code,
      )
    } finally {
      rmSync(fixture.root, { recursive: true, force: true })
    }
  }
  const offline = clientFixture({ fetch: async () => { throw new TypeError('offline') } })
  try {
    await assert.rejects(
      offline.client.readRemoval({ apiOrigin: 'https://app.whalerein.com' }),
      (error) => error.code === 'unavailable',
    )
    await assert.rejects(
      offline.client.readRemoval({ apiOrigin: '' }),
      (error) => error.code === 'invalid_argument',
    )
  } finally {
    rmSync(offline.root, { recursive: true, force: true })
  }
})

test('a response with an unparseable origin is refused before any payload is trusted', async () => {
  const fixture = clientFixture({
    fetch: async () => ({ ...response(200, { hostId: HOST_ID, state: 'enrollment_pending' }), url: '//bad-origin' }),
  })
  try {
    await assert.rejects(
      fixture.client.readStatus({ accessOrigin: ACCESS_ORIGIN }),
      (error) => error.code === 'forbidden',
    )
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('a persisted approval intent resumes without creating another request', async () => {
  let creates = 0
  const fixture = clientFixture({
    fetch: async () => {
      creates += 1
      return response(201, createdBody())
    },
  })
  try {
    const created = await fixture.client.createIntent()
    const resumed = await fixture.client.resume()
    assert.equal(resumed.intentId, created.intentId)
    assert.equal(creates, 1)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('poll resumes an uncertain create before using the missing one-time token', async () => {
  let creates = 0
  const fixture = clientFixture({
    fetch: async (url, options) => {
      if (options.method === 'POST') {
        creates += 1
        if (creates === 1) throw new TypeError('offline')
        return response(201, createdBody())
      }
      return response(200, { id: 'request-1', state: 'approved', authorization: authorization(), route: route() })
    },
  })
  try {
    await assert.rejects(fixture.client.createIntent(), (error) => error.code === 'unavailable')
    const approved = await fixture.client.poll()
    assert.equal(approved.operationId, 'op-1')
    assert.equal(creates, 2, 'the persisted create is replayed instead of minting a new intent')
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('credential validation rejects an unusable MQTT URL and a non-loopback plaintext broker', async () => {
  const base = {
    status: {
      hostId: HOST_ID,
      hostSigningPublicKey: b64(signingPublicKey),
      hostNoisePublicKey: b64(noisePublicKey),
    },
    credential: { hostId: HOST_ID, username: `host:${HOST_ID}`, password: 'x' },
  }
  const cases = ['not a url', 'mqtt://broker.example']
  for (const mqttUrl of cases) {
    const fixture = clientFixture({
      fetch: async () => response(200, { ...base, credential: { ...base.credential, mqttUrl } }),
    })
    try {
      await assert.rejects(
        fixture.client.enroll({ authorization: authorization(), route: route(), operationId: 'op-1' }),
        (error) => error.code === 'forbidden',
        mqttUrl,
      )
    } finally {
      rmSync(fixture.root, { recursive: true, force: true })
    }
  }
})

test('recovery authorization must bind this profile keys when it lists keys', async () => {
  const fixture = clientFixture({ fetch: async () => response(200, {}) })
  try {
    await assert.rejects(
      fixture.client.replaceCredential({
        accessOrigin: ACCESS_ORIGIN,
        authorization: authorization({
          purpose: 'host_credential_recovery',
          authorizationId: 'recovery-1',
          hostPublicKeys: [b64(Buffer.alloc(32, 9))],
        }),
      }),
      (error) => error.code === 'forbidden',
    )
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})

test('a denied poll clears the token through an adapter without delete', async () => {
  const credentials = new MemoryHostCredentials()
  const store = new ProfileCredentialStore({ credentials, profileId: PROFILE_ID })
  const noDelete = {
    get: (purpose) => store.get(purpose),
    set: (purpose, value) => store.set(purpose, value),
    getJson: (purpose) => store.getJson(purpose),
    setJson: (purpose, value) => store.setJson(purpose, value),
  }
  const root = mkdtempSync(join(tmpdir(), 'whalerein-enrollment-'))
  try {
    const client = new ManagedEnrollmentClient({
      apiOrigin: 'https://app.whalerein.com',
      metadata: metadata(),
      hostId: HOST_ID,
      signingPrivateKey,
      signingPublicKey,
      noisePublicKey,
      credentialStore: noDelete,
      journal: new IntentJournal({ stateDir: root }),
      fetch: async (url, options) => (options.method === 'POST'
        ? response(201, createdBody())
        : response(200, { id: 'request-1', state: 'denied' })),
      now: () => NOW,
    })
    await client.createIntent()
    await assert.rejects(client.poll(), (error) => error.code === 'denied')
    assert.equal(await noDelete.get(CREDENTIAL_PURPOSES.pollCredential), 'removed')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('a server-side expired approval is terminal and clears the one-time token', async () => {
  const fixture = clientFixture({
    fetch: async (url, options) => (options.method === 'POST'
      ? response(201, createdBody())
      : response(200, { id: 'request-1', state: 'expired' })),
  })
  try {
    assert.equal(fixture.client.metadata.service, 'whalerein-cloud')
    await fixture.client.createIntent()
    await assert.rejects(fixture.client.poll(), (error) => error.code === 'expired')
    assert.equal(await fixture.store.get(CREDENTIAL_PURPOSES.pollCredential), null)
  } finally {
    rmSync(fixture.root, { recursive: true, force: true })
  }
})
