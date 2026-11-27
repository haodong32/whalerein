import assert from 'node:assert/strict'
import test from 'node:test'

import { RouteCoordinator, RouteJournal, RouteOrderError } from '../src/access/route.js'

function journalFixture(initial = null) {
  let stored = initial
  const writes = []
  return {
    writes,
    journal: new RouteJournal({
      load: async () => stored,
      save: async (state) => {
        stored = { ...state }
        writes.push({ ...state })
      },
    }),
    stored: () => stored,
  }
}

test('route journal persists the next generation before an attach', async () => {
  const fixture = journalFixture({ generation: 4, nodeId: 'a', incarnation: 'a1', state: 'open' })
  const journal = fixture.journal
  assert.equal((await journal.open()).generation, 4)
  assert.equal(await journal.allocate('b', 'b1'), 5)
  assert.deepEqual(fixture.writes.at(-1), { generation: 5, nodeId: 'b', incarnation: 'b1', state: 'closed' })
  await journal.commit({ generation: 5, nodeId: 'b', incarnation: 'b1', state: 'open' })
  assert.equal(fixture.stored().state, 'open')
  await assert.rejects(() => journal.commit({ generation: 3, nodeId: 'a', incarnation: 'a1', state: 'open' }), RouteOrderError)
})

test('a transport loss clears the old route and attaches to the next node', async () => {
  const fixture = journalFixture()
  await fixture.journal.open()
  const calls = []
  const coordinator = new RouteCoordinator({
    journal: fixture.journal,
    retire: async () => calls.push('retire'),
    register: async (request) => {
      calls.push(`register:${request.nodeId}:${request.generation}`)
      return { outcome: 'committed', route: { generation: request.generation, nodeId: request.nodeId, incarnation: request.incarnation } }
    },
    dispatch: async (_generation, body) => body(),
  })
  const first = await coordinator.attach([{ nodeId: 'a', incarnation: 'a1', httpsUrl: 'https://a', mqttUrl: 'mqtts://a' }])
  assert.equal(first.generation, 1)
  assert.deepEqual(calls, ['retire', 'register:a:1'])
  const oldGeneration = first.generation
  calls.length = 0
  const second = await coordinator.transportLost([{ nodeId: 'b', incarnation: 'b1', httpsUrl: 'https://b', mqttUrl: 'mqtts://b' }])
  assert.equal(second.generation, 2)
  assert.deepEqual(calls, ['retire', 'register:b:2'])
  await assert.rejects(() => coordinator.dispatch(oldGeneration, async () => 'stale'), RouteOrderError)
  assert.equal(await coordinator.dispatch(second.generation, async () => 'live'), 'live')
})

test('a stale attach adopts the Central floor and retries another candidate', async () => {
  const fixture = journalFixture({ generation: 8, nodeId: 'a', incarnation: 'a1', state: 'closed' })
  await fixture.journal.open()
  const requests = []
  const coordinator = new RouteCoordinator({
    journal: fixture.journal,
    retire: async () => {},
    register: async (request) => {
      requests.push(request)
      if (request.nodeId === 'a') {
        return { outcome: 'route_stale', route: { generation: 9, nodeId: 'b', incarnation: 'b1', state: 'open' } }
      }
      return { outcome: 'committed', route: { generation: request.generation, nodeId: request.nodeId, incarnation: request.incarnation } }
    },
    dispatch: async (_generation, body) => body(),
  })
  const current = await coordinator.attach([
    { nodeId: 'a', incarnation: 'a1' },
    { nodeId: 'b', incarnation: 'b1' },
  ])
  assert.equal(current.nodeId, 'b')
  assert.equal(current.generation, 10)
  assert.equal(requests[0].generation, 9)
  assert.equal(requests[1].generation, 10)
})

test('a conflicting candidate is closed and a later candidate can win', async () => {
  const fixture = journalFixture()
  await fixture.journal.open()
  const coordinator = new RouteCoordinator({
    journal: fixture.journal,
    retire: async () => {},
    register: async (request) => {
      if (request.nodeId === 'a') return { outcome: 'conflict', reason: 'conflict' }
      return { outcome: 'idempotent', route: { generation: request.generation, nodeId: request.nodeId, incarnation: request.incarnation } }
    },
    dispatch: async (_generation, body) => body(),
  })
  const current = await coordinator.attach([{ nodeId: 'a', incarnation: 'a1' }, { nodeId: 'b', incarnation: 'b1' }])
  assert.equal(current.nodeId, 'b')
  assert.equal(fixture.stored().state, 'open')
})

test('no eligible candidate fails without opening a route', async () => {
  const fixture = journalFixture()
  await fixture.journal.open()
  const coordinator = new RouteCoordinator({
    journal: fixture.journal,
    retire: async () => {},
    register: async () => ({ outcome: 'committed' }),
    dispatch: async (_generation, body) => body(),
  })
  await assert.rejects(() => coordinator.attach([]), RouteOrderError)
  assert.equal(coordinator.current, null)
})
import { ed25519 } from '@noble/curves/ed25519.js'
import { attachInput, signRouteAttach } from '../src/access/route.js'

test('route attach canonical bytes and signature match the wire contract', () => {
  const privateKey = new Uint8Array(32).fill(7)
  const publicKey = ed25519.getPublicKey(privateKey)
  const noiseKey = new Uint8Array(32).fill(9)
  const tuple = {
    service: 'official', environment: 'staging', hostId: 'host-a', nodeId: 'node-b',
    incarnation: 'inc-1', challenge: 'challenge-1', generation: 9,
    noiseKey, signedAtUnixMs: 1760000000000,
  }
  const expected = 'whalerein/v1/host-route|attach|official|staging|host-a|node-b|inc-1|challenge-1|9|' +
    Buffer.from(noiseKey).toString('base64url') + '|1760000000000'
  assert.equal(attachInput(tuple), expected)
  const signature = signRouteAttach(privateKey, tuple)
  assert.equal(ed25519.verify(signature, new TextEncoder().encode(expected), publicKey), true)
})

test('only the lost Host moves; another Host keeps its route and old callbacks are rejected', async () => {
  const journal = () => {
    let state = { generation: 0, nodeId: '', incarnation: '', state: 'closed' }
    return { open: async () => state, snapshot: () => ({ ...state }), allocate: async (nodeId, incarnation) => {
      state = { generation: state.generation + 1, nodeId, incarnation, state: 'closed' }
      return state.generation
    }, commit: async (next) => { state = { ...next } }, floor: async (generation) => { state = { ...state, generation, state: 'closed' }; return generation } }
  }
  const register = async ({ nodeId, generation }) => ({
    outcome: 'committed',
    route: { nodeId, generation, state: 'open' },
  })
  const moved = new RouteCoordinator({ journal: journal(), register, dispatch: async (generation, body) => body() })
  const other = new RouteCoordinator({ journal: journal(), register, dispatch: async (generation, body) => body() })
  const a = { nodeId: 'node-a', incarnation: 'inc-a' }
  const b = { nodeId: 'node-b', incarnation: 'inc-b' }
  await moved.attach([a])
  await other.attach([a])
  const oldGeneration = moved.current.generation
  assert.equal(await moved.dispatch(oldGeneration, async () => 'a'), 'a')
  await moved.transportLost([b])
  assert.equal(moved.current.nodeId, 'node-b')
  await assert.rejects(moved.dispatch(oldGeneration, async () => 'stale'), { code: 'route_stale' })
  assert.equal(await moved.dispatch(moved.current.generation, async () => 'b'), 'b')
  assert.equal(other.current.nodeId, 'node-a')
  assert.equal(await other.dispatch(other.current.generation, async () => 'other-a'), 'other-a')
})
