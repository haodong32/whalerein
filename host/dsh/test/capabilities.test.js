import test from 'node:test'
import assert from 'node:assert/strict'
import { RealDshAdapter } from '../src/adapter/real.js'

function context(overrides = {}) {
  return { sessionController: { prompt: async () => ({ accepted: true }), ...overrides } }
}

test('delivery advertisement follows the callable native prompt surface, not a launcher version floor', async () => {
  const supported = new RealDshAdapter({ ctx: context({ prompt: async () => ({ accepted: true }) }), config: {} })
  assert.deepEqual(supported.supportedDeliveryModes(), ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER'])
  const legacy = new RealDshAdapter({ ctx: context({ prompt: undefined, cancel: async () => ({}) }), config: {} })
  assert.deepEqual(legacy.supportedDeliveryModes(), ['START_IF_IDLE'])
  await assert.rejects(legacy.execute('s1', { send_message: { text: 'x', delivery_mode: 'STEER' } }),
    error => error.code === 'unsupported_capability')
  await assert.rejects(new RealDshAdapter({ ctx: context({ prompt: undefined }), config: {} })
    .execute('s1', { send_message: { text: 'x' } }), error => error.code === 'unsupported_capability')
})

test('capability advertisement tracks native services that appear or disappear while the host runs', () => {
  const controller = { prompt: async () => ({ accepted: true }), state: async () => ({ queue: [] }) }
  const source = new RealDshAdapter({ ctx: { sessionController: controller }, config: {} })
  const before = source.describe().capabilities
  assert.ok(before.includes('pending-queue-read'))
  assert.equal(before.includes('pending-queue-edit'), false)
  controller.updateQueue = async () => ({})
  const edited = source.describe().capabilities
  assert.ok(edited.includes('pending-queue-edit'))
  assert.ok(edited.includes('pending-queue-remove'))
  assert.ok(edited.includes('pending-queue-steer'))
  delete controller.prompt
  const reduced = source.describe().capabilities
  assert.equal(reduced.includes('messages'), false)
  assert.deepEqual(source.supportedDeliveryModes(), ['START_IF_IDLE'])
  controller.prompt = async () => ({ accepted: true })
  assert.ok(source.describe().capabilities.includes('messages'))
  assert.deepEqual(source.supportedDeliveryModes(), ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER'])
})

test('a disabled pending queue withdraws every queue capability and rejects queue commands', async () => {
  const source = new RealDshAdapter({ ctx: context({ state: async () => ({ queue: [] }), updateQueue: async () => ({}) }),
    config: { pendingQueueEnabled: false } })
  const capabilities = source.describe().capabilities
  assert.equal(capabilities.some(name => name.startsWith('pending-queue')), false)
  await assert.rejects(source.readPendingQueue('s1'), error => error.code === 'unsupported_capability')
  await assert.rejects(source.updatePendingQueue('s1', { item_id: 'q1', action: 'REMOVE' }),
    error => error.code === 'unsupported_capability')
})
