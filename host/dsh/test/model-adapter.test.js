import test from 'node:test'
import assert from 'node:assert/strict'
import { RealDshAdapter } from '../src/adapter/real.js'
import { AdapterError, DefinitiveExecutionError } from 'whalerein-host/errors.js'

function adapter(controller) {
  return new RealDshAdapter({ ctx: { sessionController: { modelCatalog: async () => ({ default: { provider: 'default', model: 'm' } }), ...controller } } })
}

test('model catalog retains healthy providers and sanitizes failed provider errors', async () => {
  const source = adapter({ modelCatalog: async () => ({
    default: { provider: 'p', model: 'm' },
    groups: [{ id: 'p', name: 'P', models: [{ id: 'm', name: 'M', reasoning: { efforts: [{ id: 'high', name: 'High' }] } }] }],
    failures: [{ id: 'bad', name: 'Bad', message: 'secret-sentinel' }],
  }) })
  const result = await source.listModels()
  assert.equal(result.groups[0].models[0].efforts[0].id, 'high')
  assert.equal(result.effect_scope, 'session_and_source_default_attempt')
  assert.equal(result.complete, false)
  assert.deepEqual(result.failures, [{ id: 'bad', name: 'Bad', code: 'unavailable' }])
  assert.equal(JSON.stringify(result).includes('secret-sentinel'), false)
})

test('model reads use cold projections and distinguish missing selection from fallback', async () => {
  const calls = []
  const source = adapter({ projections: async request => { calls.push(request); return { kind: 'sequenced', values: { modelSelection: { next: null, lastUsed: null } } } } })
  assert.deepEqual(await source.readSessionModel('cold'), {
    pending: null, next: null, pending_known: true, last_used: null, default_selection: { provider: 'default', model: 'm' }, default_is_fallback: true,
  })
  assert.deepEqual(calls, [{ sessionId: 'cold' }])
  await assert.rejects(adapter({ projections: async () => ({ kind: 'migration-required' }) }).readSessionModel('s'), { code: 'unavailable' })
  await assert.rejects(adapter({ projections: async () => null }).readSessionModel('s'), { code: 'not_found' })
})

test('selection delegates without state gates and reports unconfirmed default persistence', async () => {
  const calls = []
  const source = adapter({ selectModel: async request => { calls.push(request); return { selected: { provider: request.provider, model: request.model, reasoningEffort: request.reasoningEffort } } } })
  assert.deepEqual(await source.selectSessionModel('busy', { provider: 'p', model: 'm', effort: 'high' }), {
    selected: { provider: 'p', model: 'm', effort: 'high' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed',
  })
  assert.deepEqual(calls, [{ sessionId: 'busy', provider: 'p', model: 'm', reasoningEffort: 'high' }])
  await source.selectSessionModel('cold', { provider: 'p', model: 'm' })
  assert.equal('reasoningEffort' in calls[1], false)
  await source.selectSessionModel('cold', { provider: 'p', model: 'm', effort: '' })
  assert.equal(calls[2].reasoningEffort, '')
})

test('model validation happens before native call; native failures are uncertain and sanitized', async () => {
  let calls = 0
  const source = adapter({ selectModel: async () => { calls++; throw new Error('secret-sentinel') } })
  await assert.rejects(source.selectSessionModel('s', { provider: '', model: 'm' }), DefinitiveExecutionError)
  assert.equal(calls, 0)
  await assert.rejects(source.selectSessionModel('s', { provider: 'p', model: 'm' }), error => error instanceof AdapterError && !(error instanceof DefinitiveExecutionError) && !error.message.includes('secret-sentinel'))
  assert.equal(calls, 1)
  await assert.rejects(adapter({ modelCatalog: undefined }).listModels(), { code: 'unsupported_capability' })
  await assert.rejects(adapter({}).readSessionModel('s'), { code: 'unsupported_capability' })
  await assert.rejects(adapter({}).selectSessionModel('s', { provider: 'p', model: 'm' }), DefinitiveExecutionError)
})

test('fake model operations are opt-in and expose typed selection without sending a prompt', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const source = new FakeDshAdapter({ sessions: [{ sessionId: 's' }], behavior: { modelOperations: true } })
  assert.ok(source.describe().capabilities.includes('model-select'))
  assert.deepEqual(await source.describeHostCapabilities(), { model_catalog: true, model_read: true, session_create: true, session_fork: true })
  assert.equal((await source.listSessions())[0].sourceCapabilities.model_selection, true)
  assert.equal((await source.readSessionModel('s')).pending, null)
  const receipt = await source.execute('s', { select_session_model: { provider: 'fixture-provider', model: 'fixture-model', effort: 'high' } })
  assert.equal(receipt.model_selection_receipt.default_persistence, 'unconfirmed')
  assert.equal((await source.readSessionModel('s')).pending.effort, 'high')
  await assert.rejects(source.selectSessionModel('s', { provider: 'bad', model: 'bad' }), DefinitiveExecutionError)
  assert.equal((await source.listModels()).groups.length, 1)
  await assert.rejects(new FakeDshAdapter().listModels(), { code: 'unsupported_capability' })
})

test('later native model projection replaces the observation without rewriting an earlier receipt', async () => {
  let pending = null
  const source = adapter({
    projections: async () => ({ kind: 'sequenced', values: { modelSelection: { next: pending, lastUsed: null } } }),
    selectModel: async request => { pending = { provider: request.provider, model: request.model }; return { selected: pending } },
  })
  const receipt = await source.selectSessionModel('s', { provider: 'p', model: 'first' })
  pending = { provider: 'p', model: 'local' }
  assert.equal((await source.readSessionModel('s')).pending.model, 'local')
  assert.equal(receipt.selected.model, 'first')
})

test('native model admission is not gated on sampled session state', async () => {
  const calls = []
  const source = adapter({
    state: () => { throw new Error('state must not be sampled') },
    selectModel: async request => { calls.push(request.sessionId); return { selected: { provider: request.provider, model: request.model } } },
  })
  for (const session of ['idle', 'running', 'queued', 'waiting-input', 'unknown', 'cold']) {
    await source.selectSessionModel(session, { provider: 'p', model: 'm' })
  }
  assert.deepEqual(calls, ['idle', 'running', 'queued', 'waiting-input', 'unknown', 'cold'])
})

test('model read keeps pending, last-used and source fallback distinct', async () => {
  const source = adapter({ projections: async () => ({ kind: 'sequenced', values: {
    modelSelection: {
      next: { provider: 'pending-provider', model: 'pending-model', reasoningEffort: 'high' },
      lastUsed: { provider: 'used-provider', model: 'used-model' },
    },
  } }) })
  const result = await source.readSessionModel('s')
  assert.deepEqual(result.pending, { provider: 'pending-provider', model: 'pending-model', effort: 'high' })
  assert.deepEqual(result.last_used, { provider: 'used-provider', model: 'used-model' })
  assert.deepEqual(result.default_selection, { provider: 'default', model: 'm' })
  assert.equal(result.default_is_fallback, false)
})

test('default_is_fallback is true only while the session has no recorded selection', async () => {
  const cold = adapter({ projections: async () => ({ kind: 'sequenced', values: { modelSelection: { next: null, lastUsed: null } } }) })
  assert.equal((await cold.readSessionModel('s')).default_is_fallback, true)
  const recorded = adapter({ projections: async () => ({ kind: 'sequenced', values: { modelSelection: {
    next: { provider: 'p', model: 'm' }, lastUsed: null,
  } } }) })
  assert.equal((await recorded.readSessionModel('s')).default_is_fallback, false)
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const fake = new FakeDshAdapter({ sessions: [{ sessionId: 's' }], behavior: { modelOperations: true } })
  assert.equal((await fake.readSessionModel('s')).default_is_fallback, true)
  await fake.selectSessionModel('s', { provider: 'fixture-provider', model: 'fixture-model' })
  assert.equal((await fake.readSessionModel('s')).default_is_fallback, false)
})


test('matching next and last-used choices leave pending status unknown', async () => {
  const choice = { provider: 'p', model: 'm' }
  const source = adapter({ projections: async () => ({ kind: 'sequenced', values: { modelSelection: { next: choice, lastUsed: choice } } }) })
  const result = await source.readSessionModel('s')
  assert.deepEqual(result.next, choice)
  assert.equal(result.pending, null)
  assert.equal(result.pending_known, false)
  await assert.rejects(adapter({ projections: async () => ({ kind: 'sequenced', values: {} }) }).readSessionModel('s'), { code: 'unavailable' })
})

test('model projection read uses the host cancellation signal', async () => {
  let received
  const source = adapter({ projections: async (_request, signal) => { received = signal; return { kind: 'sequenced', values: { modelSelection: { next: null, lastUsed: null } } } } })
  const signal = new AbortController().signal
  await source.readSessionModel('s', { signal })
  assert.equal(received, signal)
})

test('channel lifecycle stops model catalog read after cancelled projection', async () => {
  let entered,release,catalogCalls=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  const source=adapter({
    projections:async()=>{entered();await barrier;return {kind:'sequenced',values:{modelSelection:{next:null,lastUsed:null}}}},
    modelCatalog:async()=>{catalogCalls++;return {default:{provider:'p',model:'m'}}},
  })
  const abort=new AbortController()
  const pending=source.readSessionModel('s',{signal:abort.signal})
  await started
  abort.abort('unavailable')
  release()
  await assert.rejects(pending,error=>error instanceof AdapterError && error.code==='unavailable')
  assert.equal(catalogCalls,0)
  await assert.rejects(source.listModels({signal:abort.signal}),error=>error.code==='unavailable')
  assert.equal(catalogCalls,0)
})

test('channel lifecycle suppresses an already-started model catalog result', async () => {
  let entered,release
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  const source=adapter({modelCatalog:async()=>{entered();await barrier;return {groups:[]}}})
  const abort=new AbortController()
  const pending=source.listModels({signal:abort.signal})
  await started
  abort.abort('unavailable')
  release()
  await assert.rejects(pending,error=>error instanceof AdapterError && error.code==='unavailable')
})

test('fake model selection stays per-session and the next turn reports the observed model', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const source = new FakeDshAdapter({ sessions: [{ sessionId: 'a' }, { sessionId: 'b' }], behavior: { modelOperations: true } })
  const beforeB = await source.readSessionModel('b')
  await source.selectSessionModel('a', { provider: 'fixture-provider', model: 'fixture-model-2' })
  const afterB = await source.readSessionModel('b')
  // Selecting a model on one session never rewrites another session's model or
  // the source default observed by it.
  assert.equal(afterB.next, null)
  assert.equal(afterB.last_used, null)
  assert.equal(afterB.default_is_fallback, true)
  assert.deepEqual(afterB.default_selection, beforeB.default_selection)
  const afterA = await source.readSessionModel('a')
  assert.equal(afterA.next.model, 'fixture-model-2')
  assert.equal(afterA.last_used, null)
  assert.equal(afterA.default_is_fallback, false)
  // The following turn proves the source actually observed the new model, not
  // merely that a receipt was echoed.
  await source.execute('a', { send_message: { text: 'synthetic', delivery_mode: 'ENQUEUE_AFTER_RUN' } })
  const observedA = await source.readSessionModel('a')
  assert.equal(observedA.last_used.model, 'fixture-model-2')
  assert.equal(observedA.pending, null)
  assert.equal(observedA.next.model, 'fixture-model-2')
  await source.execute('b', { send_message: { text: 'synthetic', delivery_mode: 'ENQUEUE_AFTER_RUN' } })
  assert.equal((await source.readSessionModel('b')).last_used.model, beforeB.default_selection.model)
})

test('setting the same fake model twice is a no-op with an identical effective selection', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const source = new FakeDshAdapter({ sessions: [{ sessionId: 's' }], behavior: { modelOperations: true } })
  const selection = { provider: 'fixture-provider', model: 'fixture-model-2', effort: 'high' }
  const first = await source.selectSessionModel('s', selection)
  const second = await source.selectSessionModel('s', selection)
  assert.deepEqual(second.selected, first.selected)
  const read = await source.readSessionModel('s')
  assert.deepEqual(read.pending, first.selected)
  assert.equal(read.default_is_fallback, false)
})

test('fake model selection rejects an expired command before changing the session', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const source = new FakeDshAdapter({ now: () => 200, sessions: [{ sessionId: 's' }], behavior: { modelOperations: true } })
  await assert.rejects(source.selectSessionModel('s', { provider: 'fixture-provider', model: 'fixture-model' }, { expiresAt: 150 }),
    error => error.code === 'expired' && error.name === 'DefinitiveExecutionError')
  assert.equal((await source.readSessionModel('s')).next, null)
})

test('an incomplete catalog reports failed providers and refuses their selection definitively', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const source = new FakeDshAdapter({ sessions: [{ sessionId: 's' }],
    behavior: { modelOperations: true, modelFailures: [{ id: 'fixture-bad-provider', name: 'Fixture failure' }] } })
  const catalog = await source.listModels()
  assert.equal(catalog.complete, false)
  assert.deepEqual(catalog.failures, [{ id: 'fixture-bad-provider', name: 'Fixture failure', code: 'unavailable' }])
  await assert.rejects(source.selectSessionModel('s', { provider: 'fixture-bad-provider', model: 'x' }), {
    code: 'invalid_argument', name: 'DefinitiveExecutionError',
  })
  assert.equal((await source.readSessionModel('s')).next, null)
})

test('model read distinguishes an offline source from an unsupported read', async () => {
  const offline = adapter({ projections: async () => { throw Object.assign(new Error('offline'), { code: 'unavailable' }) } })
  await assert.rejects(offline.readSessionModel('s'), { code: 'unavailable' })
  const unsupported = adapter({ modelCatalog: undefined })
  await assert.rejects(unsupported.readSessionModel('s'), { code: 'unsupported_capability' })
  await assert.rejects(unsupported.selectSessionModel('s', { provider: 'p', model: 'm' }), {
    code: 'unsupported_capability', name: 'DefinitiveExecutionError',
  })
})
