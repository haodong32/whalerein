import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { ed25519, x25519 } from '@noble/curves/ed25519.js'
import { MemoryHostCredentials } from '../src/security/credentials.js'
import {
  CREDENTIAL_NAMESPACE,
  CREDENTIAL_PURPOSES,
  ProfileCredentialStore,
  createRuntimeCredentials,
  ensureRecoverySecret,
  prepareProfileIdentity,
} from '../src/credentials.js'
import { ActivationOwner } from '../src/activation.js'
import { HOST_KEY_CREDENTIAL } from '../src/security/identity.js'

const PROFILE_ID = 'a'.repeat(32)

function storeFor(credentials, profileId = PROFILE_ID, installationId = 'dsh') {
  return new ProfileCredentialStore({ credentials, profileId, installationId })
}

test('credential keys are scoped by installation and opaque profile id', async () => {
  const credentials = new MemoryHostCredentials()
  const store = storeFor(credentials)
  assert.equal(store.key(CREDENTIAL_PURPOSES.signingKey), `${CREDENTIAL_NAMESPACE}/dsh/${PROFILE_ID}/ed25519-signing-key`)
  assert.throws(() => store.key('password'), (error) => error.code === 'invalid_argument')
  assert.throws(() => new ProfileCredentialStore({ credentials, profileId: 'name' }), (error) => error.code === 'invalid_argument')
  assert.throws(() => new ProfileCredentialStore({ credentials, profileId: PROFILE_ID, installationId: '../x' }), (error) => error.code === 'invalid_argument')
  assert.throws(() => new ProfileCredentialStore({ credentials: {}, profileId: PROFILE_ID }), (error) => error.code === 'invalid_argument')
  assert.equal(await store.get(CREDENTIAL_PURPOSES.signingKey), null)
  await store.set(CREDENTIAL_PURPOSES.signingKey, 'value')
  assert.equal(await store.get(CREDENTIAL_PURPOSES.signingKey), 'value')
  await assert.rejects(store.set(CREDENTIAL_PURPOSES.signingKey, ''), (error) => error.code === 'invalid_argument')
  await store.setJson(CREDENTIAL_PURPOSES.transportCredential, { a: 1 })
  assert.deepEqual(await store.getJson(CREDENTIAL_PURPOSES.transportCredential), { a: 1 })
  await store.remove(CREDENTIAL_PURPOSES.transportCredential)
  assert.equal(await store.get(CREDENTIAL_PURPOSES.transportCredential), null)
  // Another installation/profile namespace cannot see the value.
  const other = storeFor(credentials, 'b'.repeat(32))
  assert.equal(await other.get(CREDENTIAL_PURPOSES.signingKey), null)
})

test('credential adapter failures are typed and do not leak values', async () => {
  const credentials = {
    async getCredential() {
      throw new Error('vault down')
    },
    async setCredential() {
      throw new Error('vault down')
    },
  }
  const store = storeFor(credentials)
  await assert.rejects(store.get(CREDENTIAL_PURPOSES.signingKey), (error) => error.code === 'internal')
  await assert.rejects(store.set(CREDENTIAL_PURPOSES.signingKey, 'value'), (error) => error.code === 'internal')
  await assert.rejects(store.remove(CREDENTIAL_PURPOSES.signingKey), (error) => error.code === 'internal')
  const badJson = {
    async getCredential() {
      return 'not-json'
    },
    async setCredential() {},
  }
  await assert.rejects(storeFor(badJson).getJson(CREDENTIAL_PURPOSES.transportCredential), (error) => error.code === 'internal')
})

test('profile identity creates independent signing and Noise keys and never regenerates them', async () => {
  const credentials = new MemoryHostCredentials()
  const store = storeFor(credentials)
  const first = await prepareProfileIdentity({ store, bindings: {} })
  assert.equal(first.changed, true)
  assert.equal(first.signingPrivateKey.length, 32)
  assert.equal(first.noisePrivateKey.length, 32)
  assert.notDeepEqual(Buffer.from(first.signingPrivateKey), Buffer.from(first.noisePrivateKey))
  assert.deepEqual(first.signingPublicKey, ed25519.getPublicKey(first.signingPrivateKey))
  assert.deepEqual(first.noisePublicKey, x25519.getPublicKey(first.noisePrivateKey))
  const bindings = {
    signingPublicKey: Buffer.from(first.signingPublicKey).toString('base64url'),
    noisePublicKey: Buffer.from(first.noisePublicKey).toString('base64url'),
  }
  const second = await prepareProfileIdentity({ store, bindings })
  assert.equal(second.changed, false)
  assert.deepEqual(second.signingPublicKey, first.signingPublicKey)
  assert.deepEqual(second.noisePublicKey, first.noisePublicKey)
})

test('missing or mismatched committed keys fail closed instead of rotating identity', async () => {
  const credentials = new MemoryHostCredentials()
  const store = storeFor(credentials)
  const bindings = {
    signingPublicKey: Buffer.alloc(32, 1).toString('base64url'),
    noisePublicKey: Buffer.alloc(32, 2).toString('base64url'),
  }
  await assert.rejects(prepareProfileIdentity({ store, bindings }), (error) => error.code === 'internal')
  // A corrupt credential is also a hard failure.
  await store.set(CREDENTIAL_PURPOSES.signingKey, Buffer.alloc(8).toString('base64url'))
  await assert.rejects(prepareProfileIdentity({ store, bindings: {} }), (error) => error.code === 'internal')
})

test('runtime credential adapter exposes only the committed Noise key', async () => {
  const credentials = new MemoryHostCredentials()
  const store = storeFor(credentials)
  const key = Buffer.alloc(32, 7)
  const runtime = createRuntimeCredentials({ store, noisePrivateKey: new Uint8Array(key) })
  assert.equal(await runtime.getCredential(HOST_KEY_CREDENTIAL), key.toString('base64url'))
  await assert.rejects(runtime.setCredential(HOST_KEY_CREDENTIAL, 'other'), (error) => error.code === 'internal')
  await runtime.setCredential(HOST_KEY_CREDENTIAL, key.toString('base64url'))
  await runtime.setCredential(CREDENTIAL_PURPOSES.pollCredential, 'poll')
  assert.equal(await store.get(CREDENTIAL_PURPOSES.pollCredential), 'poll')
  assert.throws(() => createRuntimeCredentials({ store, noisePrivateKey: new Uint8Array(8) }), (error) => error.code === 'invalid_argument')
})

test('recovery secret is high entropy, stable and persisted', async () => {
  const store = storeFor(new MemoryHostCredentials())
  const first = await ensureRecoverySecret(store)
  assert.ok(Buffer.from(first, 'base64url').length >= 32)
  assert.equal(await ensureRecoverySecret(store), first)
  await store.set(CREDENTIAL_PURPOSES.recoverySecret, 'bad')
  await assert.rejects(ensureRecoverySecret(store), (error) => error.code === 'internal')
})

test('installation activation ownership is exclusive and OS-released', () => {
  const root = mkdtempSync(join(tmpdir(), 'whalerein-activation-'))
  try {
    const first = new ActivationOwner({ stateRoot: root })
    const second = new ActivationOwner({ stateRoot: root })
    first.acquire()
    assert.equal(first.owned, true)
    first.acquire()
    assert.throws(() => second.acquire(), (error) => error.code === 'conflict')
    first.release()
    first.release()
    assert.equal(first.owned, false)
    second.acquire()
    assert.equal(second.owned, true)
    second.release()
    assert.throws(() => new ActivationOwner({ stateRoot: '' }), (error) => error.code === 'invalid_argument')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('activation ownership surfaces injected lock failures as conflicts', () => {
  const owner = new ActivationOwner({
    stateRoot: '/tmp/does-not-matter',
    lock: () => {
      throw new Error('locked')
    },
  })
  assert.throws(() => owner.acquire(), (error) => error.code === 'conflict')
  const pluginOwner = new ActivationOwner({
    stateRoot: '/tmp/does-not-matter',
    lock: () => {
      const error = new Error('busy')
      error.code = 'conflict'
      error.name = 'PluginError'
      throw Object.assign(error, { detail: '' })
    },
  })
  assert.throws(() => pluginOwner.acquire(), (error) => error.code === 'conflict')
})
