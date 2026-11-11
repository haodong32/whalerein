import assert from 'node:assert/strict'
import test from 'node:test'
import { RemoteError } from '@deepseek-ai/dsh-typert-protocol'
import { createInvitation } from '../src/pairing.js'
import { hostAuthMessage } from 'whalerein-host/security/host-auth.js'
import { ed25519 } from '@noble/curves/ed25519.js'

const URI = 'whalerein://pair?v=1&token=once'
const privateKey = new Uint8Array(32).fill(9)

function jsonResponse(status, body) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  }
}

async function capture(fetchImpl, options = {}) {
  try {
    return {
      value: await createInvitation({
        accessUrl: 'https://access.example',
        hostId: 'host-1',
        privateKey,
        signedAtUnixMs: 1700000000000,
        fetchImpl,
        ...options,
      }),
    }
  } catch (error) {
    return { error }
  }
}

test('when an invitation is created, Access receives the host ID and the caller gets only the URI and expiry', async () => {
  let seen
  const result = await capture(async (url, init) => {
    seen = { url: String(url), init }
    return jsonResponse(200, { pairingUri: URI, token: 'once', expiresAtUnixMs: 1700000000000 })
  })
  assert.deepEqual(result.value, { pairingUri: URI, expiresAtUnixMs: 1700000000000 })
  assert.equal(seen.url, 'https://access.example/v1/pairing/invitations')
  assert.equal(seen.init.method, 'POST')
  assert.equal(seen.init.redirect, 'error')
  assert.equal(seen.init.headers.authorization, undefined)
  const body = JSON.parse(seen.init.body)
  assert.equal(body.hostId, 'host-1')
  assert.equal(body.signedAtUnixMs, 1700000000000)
  const signature = Buffer.from(body.signature, 'base64')
  assert.equal(ed25519.verify(signature, hostAuthMessage('invitation', 'host-1', '', 1700000000000), ed25519.getPublicKey(privateKey)), true)
  assert.equal(JSON.stringify(result.value).includes('admin-secret'), false)
})

test('when the endpoint or Access response is invalid, invitation creation is rejected', async () => {
  const missingServer = await capture(async () => jsonResponse(200, {}), { accessUrl: '' })
  assert.equal(missingServer.error.code, 'whalerein/server-missing')
  const missingHost = await capture(async () => jsonResponse(200, {}), { hostId: '' })
  assert.equal(missingHost.error.code, 'whalerein/server-missing')
  const missingKey = await capture(async () => jsonResponse(200, {}), { privateKey: new Uint8Array() })
  assert.equal(missingKey.error.code, 'whalerein/host-key-missing')
  assert.ok(missingKey.error instanceof RemoteError)
  assert.equal(missingKey.error.message, missingKey.error.code)

  const cases = [
    [async () => { throw new Error('network') }, 'whalerein/unavailable'],
    [async () => jsonResponse(401, { token: 'once' }), 'whalerein/unauthorized'],
    [async () => jsonResponse(403, { token: 'once' }), 'whalerein/unauthorized'],
    [async () => jsonResponse(410, { token: 'once' }), 'whalerein/unauthorized'],
    [async () => jsonResponse(404, { token: 'once' }), 'whalerein/host-not-found'],
    [async () => jsonResponse(500, {}), 'whalerein/unavailable'],
    [async () => ({ ok: true, status: 200, json: async () => { throw new Error('bad json') } }), 'whalerein/unavailable'],
    [async () => jsonResponse(200, { pairingUri: 'https://example.invalid' }), 'whalerein/unavailable'],
  ]
  for (const [fetchImpl, code] of cases) {
    const result = await capture(fetchImpl)
    assert.equal(result.error.code, code)
    assert.equal(JSON.stringify(result.error.details), '{}')
    assert.equal(result.error.message.includes('once'), false)
  }

  const missingExpiry = await capture(async () => jsonResponse(200, { pairingUri: URI, expiresAtUnixMs: 'soon' }))
  assert.equal(missingExpiry.value.expiresAtUnixMs, 0)
})
