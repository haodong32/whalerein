import test from 'node:test'
import assert from 'node:assert/strict'
import { normalizeQuestionItems, validateQuestionAnswers, QuestionHandles } from '../src/adapter/questions.js'

const questions = [{ id: 'q', question: 'Pick', detail: 'Context', header: 'Heading', multiSelect: true,
  options: [{ label: 'A', description: 'First' }, { label: 'B' }],
  intent: { kind: 'plan-review', approve: 'A', callId: 'call', subject: { name: 'sample', icon: 'icon' },
    display: { question: { en: 'Question' }, detail: { en: 'Context' }, approve: { en: 'Accept' } } } }]

test('normalizes available native context and preserves labels and optional answer text', () => {
  const [item] = normalizeQuestionItems(questions)
  assert.equal(item.detail, 'Context')
  assert.equal(item.header, 'Heading')
  assert.equal(item.options[0].description, 'First')
  assert.equal(item.intent.call_id, 'call')
  assert.deepEqual(item.intent.display.question.translations, { en: 'Question' })
  assert.deepEqual(validateQuestionAnswers([item], [{ id: 'q', selected: ['A', 'B'], custom: '' }]),
    [{ id: 'q', selected: ['A', 'B'], custom: '' }])
  assert.deepEqual(normalizeQuestionItems([{ id: 'x', question: 'x' }])[0].options, [])
  assert.deepEqual(validateQuestionAnswers([{ id: 'x', options: [], multi_select: false }], [{ id: 'x', selected: [] }]), [{ id: 'x', selected: [] }])
  for (const answer of [
    { id: 'q', selected: ['unoffered'] },
    { id: 'q', selected: ['A', 'A'] },
    { id: 'q', selected: ['A', 'B'] },
    { id: 'q', selected: [], custom: 'fixture' },
  ]) {
    assert.deepEqual(validateQuestionAnswers([{ ...item, multi_select: false, intent: { kind: 'approval' } }], [answer]), [answer])
  }
})

test('invalid projection and incomplete or invalid answers do not reach native code', () => {
  for (const input of [null, [{}], [{ id: 'x', question: 'q', options: [{}] }]]) {
    assert.throws(() => normalizeQuestionItems(input))
  }
  const [multi] = normalizeQuestionItems(questions)
  for (const [items, answer] of [
    [[multi], []], [[multi], [{ id: 'wrong', selected: [] }]],
    [[multi], [{ id: 'q', selected: [], custom: 1 }]],
    [[multi, multi], [{ id: 'q', selected: [] }, { id: 'q', selected: [] }]],
    [[multi], [{ id: 'q', selected: [1] }]], [[multi], [{ id: 'q' }]],
  ]) assert.throws(() => validateQuestionAnswers(items, answer), { code: 'invalid_argument' })
})

test('live handles are scoped, release once, and cannot release a later handle', () => {
  const registry = new QuestionHandles()
  const events = []
  const unsubscribe = registry.subscribe(event => events.push(event))
  registry.subscribe(() => { throw new Error('observer') })
  let disposals = 0
  assert.throws(() => registry.register('', 'call', () => {}))
  const first = registry.register('session', 'call', () => { disposals++ })
  assert.throws(() => registry.register('session', 'call', () => {}), { code: 'conflict' })
  assert.deepEqual(registry.listActive('other'), [])
  assert.deepEqual(registry.listActive('session'), [{ sessionId: 'session', batchId: first.batchId }])
  assert.equal(first.release('resolved'), true)
  const second = registry.register('session', 'call', () => { disposals++ })
  assert.equal(first.release(), false)
  assert.equal(registry.listActive('session').length, 1)
  registry.close()
  assert.equal(second.release(), false)
  assert.equal(disposals, 2)
  assert.deepEqual(events.map(event => event.type), ['requested', 'resolved', 'requested', 'released'])
  unsubscribe()
})

test('projected batches are guarded, session scoped, and never manufacture live handles', async () => {
  const { QuestionProjection } = await import('../src/adapter/questions.js')
  const projection = new QuestionProjection()
  const native = { active: [{ callId: 'native', state: 'open', questions }], settled: [] }
  const first = projection.project('one', native)[0]
  const again = projection.project('one', native)[0]
  assert.equal(first.batch_id, again.batch_id)
  assert.equal(first.version, again.version)
  assert.deepEqual(first.digest, again.digest)
  assert.equal(first.answerable, false)
  assert.notEqual(projection.project('two', native)[0].batch_id, first.batch_id)
  native.active[0].state = 'continued'
  const changed = projection.project('one', native)[0]
  assert.equal(changed.batch_id, first.batch_id)
  assert.ok(changed.version > first.version)
  assert.notDeepEqual(changed.digest, first.digest)
  projection.project('one', { active: [] })
  assert.notEqual(projection.project('one', native)[0].batch_id, first.batch_id)
  projection.clear()
  assert.notEqual(projection.project('one', native)[0].source_epoch, first.source_epoch)
  for (const invalid of [null, {}, { active: [{}] }, { active: [{ callId: 'a', state: 'unknown' }] },
    { active: [native.active[0], native.active[0]] }]) assert.throws(() => projection.project('one', invalid))
})

test('real adapter reads the exact projection without activating or inventing a provider handle', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  let calls = 0
  let result = { kind: 'sequenced', values: { userQuestions: { active: [{ callId: 'c', state: 'open', questions }], settled: [] } } }
  const adapter = new RealDshAdapter({ ctx: { agents: { get: () => undefined, roots: () => [] }, sessionController: { async projections(request) {
    calls++
    assert.equal(request.sessionId, 'session')
    return result
  } } } })
  const rows = await adapter.listProjectedQuestions('session')
  assert.equal(calls, 1)
  assert.equal(rows[0].answerable, false)
  assert.deepEqual(adapter.listClaimedQuestions('session'), [])
  const off = adapter.subscribeQuestions(() => { throw new Error('unexpected event') })
  off()
  result = null
  await assert.rejects(adapter.listProjectedQuestions('session'), { code: 'not_found' })
  result = { kind: 'migration-required', values: {} }
  await assert.rejects(adapter.listProjectedQuestions('session'), { code: 'unavailable' })
  await assert.rejects(adapter.listProjectedQuestions(''), { code: 'invalid_argument' })
  await assert.rejects(new RealDshAdapter({ ctx: {} }).listProjectedQuestions('session'), { code: 'unsupported_capability' })
  await adapter.close()
})

test('continued candidate checks source identity and guard before one native call', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  const session = {}, agent = { id: 'session', session, inbox: { nextTurn: [], nextStep: [] } }
  let root = true
  let answerCalls = 0
  let nativeResult = true
  const ctx = {
    sessions: { get: () => session },
    sessionController: { projections: async () => ({ kind: 'sequenced', values: { userQuestions: {
      active: [{ callId: 'native', state: 'continued', questions }], settled: [],
    } } }) },
    agents: { get: () => agent, roots: () => root ? [agent] : [] },
    userQuestions: { answer: (owner, callId, payload) => {
      answerCalls++
      assert.equal(owner, agent)
      assert.equal(callId, 'native')
      assert.deepEqual(payload, { answers: [{ id: 'q', selected: ['A'] }] })
      if (nativeResult instanceof Error) throw nativeResult
      return nativeResult
    } },
  }
  const adapter = new RealDshAdapter({ ctx })
  const batch = (await adapter.listProjectedQuestions('session'))[0]
  const request = { session_id: 'session', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  await assert.rejects(adapter.answerContinuedQuestion({ ...request, source_epoch: 'stale' }), { code: 'conflict' })
  await assert.rejects(adapter.answerContinuedQuestion({ ...request, answers: [] }), { code: 'invalid_argument' })
  root = false
  await assert.rejects(adapter.answerContinuedQuestion(request), { code: 'unsupported_capability' })
  assert.equal(answerCalls, 0)
  root = true
  assert.deepEqual(await adapter.answerContinuedQuestion(request), { batch_id: batch.batch_id, phase: 'QUESTION_REPLY_QUEUED' })
  nativeResult = false
  await assert.rejects(adapter.answerContinuedQuestion(request), { code: 'conflict' })
  nativeResult = new Error('private native error')
  await assert.rejects(adapter.answerContinuedQuestion(request), error => error === nativeResult)
  assert.equal(answerCalls, 3)
})

test('fake continued questions use guarded fixture state and remain optional', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const adapter = new FakeDshAdapter({ behavior: { questions: true }, sessions: [{ sessionId: 's' }] })
  adapter.setFixtureQuestion('s', { callId: 'call', state: 'continued', questions })
  const session = (await adapter.listSessions())[0]
  assert.equal(session.sourceCapabilities.questions_answer_continued, true)
  assert.equal(session.sourceCapabilities.questions_answer_open, false)
  const page = await adapter.listQuestionPage('s')
  const batch = page.batches[0]
  assert.equal(batch.answerable, true)
  await assert.rejects(adapter.listQuestionPage('s', { cursor: 'unexpected' }), { code: 'invalid_argument' })
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  await assert.rejects(adapter.answerContinuedQuestion({ ...request, answers: [] }), { code: 'invalid_argument' })
  await assert.rejects(adapter.answerContinuedQuestion({ ...request, source_epoch: 'old' }), { code: 'conflict' })
  assert.equal((await adapter.execute('s', { answer_question: request })).question_receipt.phase, 'QUESTION_REPLY_QUEUED')
  assert.deepEqual((await adapter.listQuestionPage('s')).batches, [])
  await assert.rejects(adapter.answerContinuedQuestion(request), { code: 'conflict' })
  await assert.rejects(adapter.execute('s', { answer_question: { ...request, session_id: 'other' } }), { code: 'invalid_argument' })
  await assert.rejects(adapter.execute('s', { answer_question: { ...request, expected_phase: 'QUESTION_OPEN' } }), { code: 'unsupported_capability' })
  const disabled = new FakeDshAdapter({ sessions: [{ sessionId: 's' }] })
  assert.throws(() => disabled.setFixtureQuestion('s', {}), { code: 'unsupported_capability' })
  await assert.rejects(disabled.listQuestionPage('s'), { code: 'unsupported_capability' })
})

test('handle release failure is visible and does not prevent other teardown', () => {
  const registry = new QuestionHandles()
  const events = []
  registry.subscribe(event => events.push(event.type))
  const first = registry.register('s', 'one', () => { throw new Error('private') })
  assert.throws(() => first.release(), { code: 'unavailable' })
  let cleaned = false
  registry.register('s', 'two', () => { throw new Error('private') })
  registry.register('s', 'three', () => { cleaned = true })
  registry.close()
  assert.equal(cleaned, true)
  assert.deepEqual(events, ['requested', 'release_failed', 'requested', 'requested', 'release_failed', 'released'])
})

test('optional Cordis question services follow their injection scope', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  const session = {}, agent = { id: 's', session, inbox: { nextTurn: [], nextStep: [] } }
  let cleanup
  let disposed = 0
  const ctx = {
    sessions: { get: () => session },
    sessionController: { projections: async () => ({ kind: 'sequenced', values: { userQuestions: {
      active: [{ callId: 'native', state: 'continued', questions }],
    } } }) },
    inject(names, callback) {
      assert.deepEqual(names, ['agents', 'userQuestions'])
      cleanup = callback({ agents: { get: () => agent, roots: () => [agent] }, userQuestions: { answer: () => true } })
      return { dispose() { disposed++; cleanup() } }
    },
  }
  const adapter = new RealDshAdapter({ ctx })
  await adapter.open()
  const batch = (await adapter.listProjectedQuestions('s'))[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  assert.equal((await adapter.answerContinuedQuestion(request)).phase, 'QUESTION_REPLY_QUEUED')
  await adapter.close()
  assert.equal(disposed, 1)
  await assert.rejects(adapter.answerContinuedQuestion(request), { code: 'unsupported_capability' })
})

test('open broker runs local provider immediately and settles only one racing answer', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: id => id === 's' ? agent : undefined, roots: () => [agent] }
  const handles = new QuestionHandles(), events = []
  handles.subscribe(event => events.push(event.type))
  let claims = 0
  const userQuestions = { async *attachWait(owner, callId, signal) {
    assert.equal(owner, agent); assert.equal(callId, 'call'); claims++
    try { yield { remainingMs: 1000 }; await new Promise(resolve => signal.addEventListener('abort', resolve, { once: true })) }
    finally { claims-- }
  } }
  const broker = new OpenQuestionBroker({ agents, userQuestions, handles })
  const source = new AbortController(), local = Promise.withResolvers()
  let localCalled = false
  const request = { agent, questions, wait: { callId: 'call', timed: true }, signal: source.signal }
  const native = broker.capture(request, () => { localCalled = true; return local.promise })
  await Promise.resolve()
  assert.equal(localCalled, true)
  assert.deepEqual(handles.listActive('s'), [])
  const batch = broker.list('s')[0]
  const answer = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  await assert.rejects(broker.answer({ ...answer, source_epoch: 'old' }), { code: 'conflict' })
  const receipt = await broker.answer(answer)
  assert.equal(receipt.phase, 'QUESTION_SETTLED')
  assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['A'] }] })
  assert.equal(claims, 0)
  assert.deepEqual(events, ['requested', 'released'])
  await assert.rejects(broker.answer(answer), { code: 'conflict' })
  local.resolve({ answers: [{ id: 'q', selected: ['B'] }] })
  assert.deepEqual(broker.list('s'), [])
})

test('open broker preserves local first, explicit cancellation and native scope', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: id => id === 's' ? agent : undefined, roots: () => [agent] }
  const handles = new QuestionHandles()
  let attachCalls = 0
  const userQuestions = { async *attachWait(_agent, _call, signal) {
    attachCalls++
    await new Promise(resolve => signal.addEventListener('abort', resolve, { once: true }))
  } }
  const broker = new OpenQuestionBroker({ agents, userQuestions, handles })
  const nativeSignal = new AbortController(), local = Promise.withResolvers()
  const native = broker.capture({ agent, questions, wait: { callId: 'one', timed: true }, signal: nativeSignal.signal }, () => local.promise)
  const batch = broker.list('s')[0]
  const input = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  const cancelled = new AbortController()
  const attempt = broker.answer(input, cancelled.signal)
  await Promise.resolve(); cancelled.abort()
  await assert.rejects(attempt, { code: 'unavailable' })
  assert.equal(attachCalls, 1)
  assert.deepEqual(handles.listActive('s'), [])
  local.resolve({ answers: [{ id: 'q', selected: ['B'] }] })
  assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['B'] }] })
  await assert.rejects(broker.answer(input), { code: 'conflict' })
  const other = await broker.capture({ agent: { id: 's' }, questions, wait: { callId: 'wrong' } }, async () => 'delegate')
  assert.equal(other, 'delegate')
  assert.deepEqual(broker.list('s'), [])
})

test('open broker delegates on teardown and does not prolong indefinite no-provider requests', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: () => agent, roots: () => [agent] }
  const broker = new OpenQuestionBroker({ agents, userQuestions: {}, handles: new QuestionHandles() })
  const local = Promise.withResolvers()
  const request = { agent, questions, wait: { callId: 'x' } }
  const native = broker.capture(request, () => local.promise)
  broker.close()
  assert.equal(request.signal.aborted, false)
  local.resolve('local')
  assert.equal(await native, 'local')
  assert.equal(await broker.capture({ agent, questions, wait: { callId: 'later' } }, async () => 'delegate'), 'delegate')
  const fresh = new OpenQuestionBroker({ agents, userQuestions: {}, handles: new QuestionHandles() })
  await assert.rejects(fresh.capture({ agent, questions, wait: { callId: 'indefinite' } }, async () => { throw Object.assign(new Error('unavailable'), { code: 'NO_PROVIDER' }) }), { code: 'NO_PROVIDER' })
})

test('remote settlement cancels only the delegated local provider signal', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: () => agent, roots: () => [agent] }
  const broker = new OpenQuestionBroker({ agents, userQuestions: {}, handles: new QuestionHandles() })
  const request = { agent, questions }
  let localAborted = false
  const native = broker.capture(request, () => new Promise((_, reject) => {
    request.signal.addEventListener('abort', () => { localAborted = true; reject(new Error('local released')) }, { once: true })
  }))
  await Promise.resolve()
  const batch = broker.list('s')[0]
  const answer = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  assert.equal((await broker.answer(answer)).phase, 'QUESTION_SETTLED')
  assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['A'] }] })
  assert.equal(localAborted, true)
})

test('opt-in open broker exposes live context without advertising unverified capability', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  const agent = { id: 's' }, local = Promise.withResolvers()
  let provider, cleanup, off = false
  const scope = {
    agents: { get: id => id === 's' ? agent : undefined, roots: () => [agent] },
    userQuestions: { async *attachWait(_agent, _call, signal) {
      yield { remainingMs: 1000 }
      await new Promise(resolve => signal.addEventListener('abort', resolve, { once: true }))
    } },
    on(name, callback) { assert.equal(name, 'user-questions/request'); provider = callback; return () => { off = true } },
  }
  const ctx = {
    sessionController: { projections: async () => ({ kind: 'sequenced', values: { userQuestions: {
      active: [{ callId: 'call', state: 'open', questions }],
    } } }) },
    inject(_names, callback) { cleanup = callback(scope); return { dispose: () => cleanup() } },
  }
  const adapter = new RealDshAdapter({ ctx, config: { enableOpenQuestionBroker: true } })
  await adapter.open()
  assert.equal(adapter.describe().capabilities.includes('questions-answer-open'), true)
  const native = provider({ agent, questions, wait: { callId: 'call', timed: true }, signal: new AbortController().signal }, () => local.promise)
  const page = await adapter.listQuestionPage('s')
  assert.equal(page.batches.length, 1)
  assert.equal(page.batches[0].answerable, true)
  const batch = page.batches[0]
  assert.equal((await adapter.getQuestion('s', batch.batch_id)).batch_id, batch.batch_id)
  const answer = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  assert.equal((await adapter.answerOpenQuestion(answer)).phase, 'QUESTION_SETTLED')
  assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['A'] }] })
  local.resolve('ignored')
  await assert.rejects(adapter.listQuestionPage('s', { cursor: 'invalid' }), { code: 'invalid_argument' })
  await adapter.close()
  assert.equal(off, true)
  await assert.rejects(adapter.answerOpenQuestion(answer), { code: 'unsupported_capability' })
})

test('open broker releases an in-flight claim on disconnect and lets local provider finish', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: () => agent, roots: () => [agent] }
  const handles = new QuestionHandles(), local = Promise.withResolvers(), gate = Promise.withResolvers()
  let nativeClaims = 0
  const userQuestions = { async *attachWait(_agent, _call, signal) {
    nativeClaims++
    try { await gate.promise; if (!signal.aborted) yield { remainingMs: 1000 } }
    finally { nativeClaims-- }
  } }
  const broker = new OpenQuestionBroker({ agents, userQuestions, handles })
  const native = broker.capture({ agent, questions, wait: { callId: 'call', timed: true }, signal: new AbortController().signal }, () => local.promise)
  const batch = broker.list('s')[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  const channel = new AbortController()
  const remote = broker.answer(request, channel.signal)
  await Promise.resolve()
  assert.equal(nativeClaims, 1)
  channel.abort(); gate.resolve()
  await assert.rejects(remote, { code: 'unavailable' })
  assert.equal(nativeClaims, 0)
  assert.deepEqual(handles.listActive('s'), [])
  local.resolve({ answers: [{ id: 'q', selected: ['B'] }] })
  assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['B'] }] })
  await assert.rejects(broker.answer(request), { code: 'conflict' })
})

test('opt-in fake open questions settle explicitly and reject cancellation', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const adapter = new FakeDshAdapter({ behavior: { questions: true, questionsOpen: true }, sessions: [{ sessionId: 's' }] })
  adapter.setFixtureQuestion('s', { callId: 'call', state: 'open', questions })
  const batch = (await adapter.listQuestionPage('s')).batches[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  const cancelled = new AbortController(); cancelled.abort()
  await assert.rejects(adapter.execute('s', { answer_question: request }, { signal: cancelled.signal }), { code: 'unavailable' })
  await assert.rejects(adapter.answerOpenQuestion({ ...request, answers: [] }), { code: 'invalid_argument' })
  await assert.rejects(adapter.answerOpenQuestion({ ...request, source_epoch: 'old' }), { code: 'conflict' })
  assert.equal((await adapter.execute('s', { answer_question: request })).question_receipt.phase, 'QUESTION_SETTLED')
  await assert.rejects(adapter.answerOpenQuestion(request), { code: 'conflict' })
})

test('fixture held open answer cancels without consuming the question', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const adapter = new FakeDshAdapter({ behavior: { questions: true, questionsOpen: true }, sessions: [{ sessionId: 's' }] })
  adapter.setFixtureQuestion('s', { callId: 'call', state: 'open', questions })
  adapter.holdFixtureQuestion('s')
  const batch = (await adapter.listQuestionPage('s')).batches[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  const controller = new AbortController()
  const pending = adapter.answerOpenQuestion(request, controller.signal)
  while (!adapter.fixtureQuestionWaiting('s')) await new Promise(resolve => setImmediate(resolve))
  controller.abort()
  await assert.rejects(pending, { code: 'unavailable' })
  assert.equal(adapter.fixtureQuestionWaiting('s'), false)
  assert.equal((await adapter.listQuestionPage('s')).batches.length, 1)
})

test('real question provider is registered by default and can be opted out', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  let listeners = 0
  const scope = { agents: {}, userQuestions: {}, on() { listeners++; return () => {} } }
  const ctx = { sessionController: {}, inject(_names, callback) { const dispose = callback(scope); return { dispose } } }
  const adapter = new RealDshAdapter({ ctx })
  await adapter.open()
  assert.equal(listeners, 1)
  assert.deepEqual(adapter.listClaimedQuestions('s'), [])
  assert.equal(adapter.describe().capabilities.includes('questions-answer-open'), true)
  await adapter.close()
  listeners = 0
  const disabled = new RealDshAdapter({ ctx, config: { enableOpenQuestionBroker: false } })
  await disabled.open()
  assert.equal(listeners, 0)
  assert.equal(disabled.describe().capabilities.includes('questions-answer-open'), false)
  await disabled.close()
})

test('live open replacements retain native timed projection order', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  const agent = { id: 's' }, waiting = []
  let provider, cleanup
  const scope = { agents: { get: () => agent, roots: () => [agent] }, userQuestions: {},
    on(_name, callback) { provider = callback; return () => {} } }
  const q = id => [{ id, question: 'Fixture' }]
  const ctx = { sessionController: { projections: async () => ({ kind: 'sequenced', values: { userQuestions: {
    active: [{ callId: 'a', state: 'open', questions: q('a') }, { callId: 'b', state: 'continued', questions: q('b') },
      { callId: 'c', state: 'open', questions: q('c') }],
  } } }) }, inject(_names, callback) { cleanup = callback(scope); return { dispose: () => cleanup() } } }
  const adapter = new RealDshAdapter({ ctx, config: { enableOpenQuestionBroker: true } })
  await adapter.open()
  for (const id of ['c', 'a']) {
    const local = Promise.withResolvers()
    waiting.push(local)
    provider({ agent, questions: q(id), wait: { callId: id, timed: true }, signal: new AbortController().signal }, () => local.promise)
  }
  const page = await adapter.listQuestionPage('s')
  assert.deepEqual(page.batches.map(batch => batch.questions[0].id), ['a', 'b', 'c'])
  assert.deepEqual(page.batches.map(batch => batch.answerable), [true, false, true])
  await adapter.close()
  for (const local of waiting) local.resolve({ answers: [] })
})

test('an open answer arriving during native claim acquisition loses to local settlement', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: () => agent, roots: () => [agent] }
  const handles = new QuestionHandles(), local = Promise.withResolvers(), gate = Promise.withResolvers()
  let claims = 0
  const userQuestions = { async *attachWait() {
    claims++
    try { await gate.promise; yield { remainingMs: 1000 } } finally { claims-- }
  } }
  const broker = new OpenQuestionBroker({ agents, userQuestions, handles })
  const native = broker.capture({ agent, questions, wait: { callId: 'call', timed: true }, signal: new AbortController().signal }, () => local.promise)
  const batch = broker.list('s')[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  const remote = broker.answer(request)
  await Promise.resolve()
  local.resolve({ answers: [{ id: 'q', selected: ['B'] }] })
  assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['B'] }] })
  gate.resolve()
  await assert.rejects(remote, { code: 'conflict' })
  assert.equal(claims, 0)
  assert.deepEqual(handles.listActive('s'), [])
})

test('source cancellation removes open handles and a stale answer cannot settle a later call', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: () => agent, roots: () => [agent] }
  const broker = new OpenQuestionBroker({ agents, userQuestions: {}, handles: new QuestionHandles() })
  const controller = new AbortController(), local = Promise.withResolvers()
  const native = broker.capture({ agent, questions, wait: { callId: 'same' }, signal: controller.signal }, () => local.promise)
  const batch = broker.list('s')[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  controller.abort(new Error('cancelled'))
  await assert.rejects(native, /cancelled/)
  const next = broker.capture({ agent, questions, wait: { callId: 'same' } }, () => Promise.resolve({ answers: [] }))
  const later = broker.list('s')[0]
  assert.notEqual(later.batch_id, batch.batch_id)
  await assert.rejects(broker.answer(request), { code: 'conflict' })
  await next
})

test('blocking approval intent without a native wait keeps live context and never fabricates a timed claim', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: () => agent, roots: () => [agent] }
  const handles = new QuestionHandles(), local = Promise.withResolvers()
  let claims = 0
  const broker = new OpenQuestionBroker({ agents, handles, userQuestions: { attachWait() { claims++; throw new Error('unexpected') } } })
  const approval = [{ id: 'q', question: 'Fixture', detail: 'Requested scope', options: [{ label: 'Allow' }, { label: 'Deny' }],
    intent: { kind: 'approval', approve: 'Allow', reject: 'Deny', subject: { name: 'Subject' }, display: { question: { en: 'Question' } } } }]
  const native = broker.capture({ agent, questions: approval, signal: new AbortController().signal }, () => local.promise)
  const batch = broker.list('s')[0]
  assert.equal(batch.questions[0].detail, 'Requested scope')
  assert.equal(batch.questions[0].intent.kind, 'approval')
  assert.deepEqual(batch.questions[0].intent.display.question.translations, { en: 'Question' })
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['Deny'] }] }
  assert.equal((await broker.answer(request)).phase, 'QUESTION_SETTLED')
  assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['Deny'] }] })
  assert.equal(claims, 0)
  assert.deepEqual(handles.listActive('s'), [])
  local.resolve({ answers: [] })
})

test('owned child and ownership changes cannot expose projected questions', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  const root = { id: 's' }, child = { id: 's' }
  let current = child, reads = 0
  const ctx = { agents: { get: () => current, roots: () => [root] }, sessionController: {
    async projections() { reads++; current = child; return { kind: 'sequenced', values: { userQuestions: {
      active: [{ callId: 'secret', state: 'continued', questions }],
    } } } },
  } }
  const adapter = new RealDshAdapter({ ctx })
  await assert.rejects(adapter.listProjectedQuestions('s'), { code: 'unsupported_capability' })
  assert.equal(reads, 0)
  current = root
  await assert.rejects(adapter.listProjectedQuestions('s'), { code: 'unsupported_capability' })
  assert.equal(reads, 1)
})

test('legacy live questions interleave with timed projections by native source position', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  const session = { seq: 5, snapshotEvents: () => [
    { seq: 2, type: 'tool/call', data: { callId: 'a' } },
    { seq: 7, type: 'tool/ptc-dispatch', data: { subCallId: 'b' } },
  ] }
  const agent = { id: 's', session }, local = Promise.withResolvers()
  let provider, cleanup, visible = true
  const scope = { agents: { get: () => agent, roots: () => [agent] }, userQuestions: {},
    on(_name, callback) { provider = callback; return () => {} } }
  const item = id => [{ id, question: 'Fixture' }]
  const ctx = { sessions: { get: () => visible ? session : undefined }, sessionController: { projections: async () => ({
    kind: 'sequenced', asOfSeq: 7, values: { userQuestions: { active: [
      { callId: 'a', state: 'continued', questions: item('a') },
      { callId: 'b', state: 'continued', questions: item('b') },
    ] } },
  }) }, inject(_names, callback) { cleanup = callback(scope); return { dispose: () => cleanup() } } }
  const adapter = new RealDshAdapter({ ctx, config: { enableOpenQuestionBroker: true } })
  await adapter.open()
  const native = provider({ agent, questions: item('live'), signal: new AbortController().signal }, () => local.promise)
  const page = await adapter.listQuestionPage('s')
  assert.deepEqual(page.batches.map(batch => batch.questions[0].id), ['a', 'live', 'b'])
  visible = false
  await assert.rejects(adapter.listQuestionPage('s'), { code: 'unavailable' })
  local.resolve({ answers: [] }); await native; await adapter.close()
})

test('fake question list preserves other batches when one continued answer is submitted', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const adapter = new FakeDshAdapter({ behavior: { questions: true }, sessions: [{ sessionId: 's' }] })
  adapter.setFixtureQuestions('s', ['one', 'two', 'three'].map(callId => ({ callId, state: 'continued', questions })))
  const before = (await adapter.listQuestionPage('s')).batches
  const batch = before[1]
  const answer = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  assert.equal((await adapter.answerContinuedQuestion(answer)).phase, 'QUESTION_REPLY_QUEUED')
  assert.deepEqual((await adapter.listQuestionPage('s')).batches.map(item => item.batch_id), [before[0].batch_id, before[2].batch_id])
  assert.throws(() => adapter.setFixtureQuestions('s', null), { code: 'unsupported_capability' })
})

test('continued answer rechecks the deadline after projection and never invokes an expired source', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  const session = {}, agent = { id: 's', session, inbox: { nextTurn: [], nextStep: [] } }, gate = Promise.withResolvers()
  let now = 100, block = false, calls = 0
  const ctx = { sessions: { get: () => session }, agents: { get: () => agent, roots: () => [agent] }, userQuestions: { answer: () => { calls++; return true } },
    sessionController: { async projections() {
      if (block) await gate.promise
      return { kind: 'sequenced', values: { userQuestions: { active: [{ callId: 'call', state: 'continued', questions }] } } }
    } } }
  const adapter = new RealDshAdapter({ ctx, now: () => now })
  const batch = (await adapter.listProjectedQuestions('s'))[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  block = true
  const attempt = adapter.answerContinuedQuestion(request, { expiresAt: 150 })
  now = 150; gate.resolve()
  await assert.rejects(attempt, error => error.code === 'expired' && error.name === 'DefinitiveExecutionError')
  assert.equal(calls, 0)
  block = false
  ctx.userQuestions.answer = () => { calls++; now = 200; return true }
  now = 100
  assert.equal((await adapter.answerContinuedQuestion(request, { expiresAt: 150 })).phase, 'QUESTION_REPLY_QUEUED')
  assert.equal(calls, 1)
})

test('open answer rechecks the deadline after native claim before settling', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: () => agent, roots: () => [agent] }
  const handles = new QuestionHandles(), gate = Promise.withResolvers(), local = Promise.withResolvers()
  let now = 100, claims = 0
  const userQuestions = { async *attachWait() {
    claims++
    try { await gate.promise; yield { remainingMs: 1000 } } finally { claims-- }
  } }
  const broker = new OpenQuestionBroker({ agents, userQuestions, handles, now: () => now })
  const native = broker.capture({ agent, questions, wait: { callId: 'call', timed: true } }, () => local.promise)
  const batch = broker.list('s')[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  const attempt = broker.answer(request, undefined, { expiresAt: 150 })
  await Promise.resolve(); now = 150; gate.resolve()
  await assert.rejects(attempt, error => error.code === 'expired' && error.name === 'DefinitiveExecutionError')
  assert.equal(claims, 0)
  assert.deepEqual(handles.listActive('s'), [])
  local.resolve({ answers: [{ id: 'q', selected: ['B'] }] })
  assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['B'] }] })
})

test('continued eligibility follows exact live root and native inbox without inventing source state', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  const session = {}, agent = { id: 's', session, inbox: { nextTurn: [], nextStep: [] } }
  let current = agent, nativeError = null
  const ctx = {
    agents: { get: () => current, roots: () => [agent] }, sessions: { get: () => session },
    userQuestions: { answer: () => { if (nativeError) throw nativeError; return true } },
    sessionController: { projections: async () => ({ kind: 'sequenced', asOfSeq: 4, values: {
      userQuestions: { active: [{ callId: 'call', state: 'continued', questions }] },
    } }) },
  }
  const adapter = new RealDshAdapter({ ctx })
  const before = (await adapter.listQuestionPage('s')).batches[0]
  assert.equal(before.phase, 'QUESTION_CONTINUED'); assert.equal(before.answerable, true)
  const request = { session_id: 's', batch_id: before.batch_id, source_epoch: before.source_epoch,
    expected_version: before.version, expected_digest: before.digest, expected_phase: before.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  agent.inbox.nextStep.push({ source: { kind: 'user-question-reply', callId: 'call' } })
  const queued = (await adapter.listQuestionPage('s')).batches[0]
  assert.equal(queued.phase, 'QUESTION_REPLY_QUEUED'); assert.equal(queued.answerable, false)
  assert.ok(queued.version > before.version); assert.notDeepEqual(queued.digest, before.digest)
  await assert.rejects(adapter.answerContinuedQuestion(request), { code: 'conflict', name: 'DefinitiveExecutionError' })
  agent.inbox.nextStep.length = 0
  const restored = (await adapter.listQuestionPage('s')).batches[0]
  assert.equal(restored.phase, 'QUESTION_CONTINUED'); assert.equal(restored.answerable, true)
  assert.ok(restored.version > queued.version)
  const latest = { ...request, expected_version: restored.version, expected_digest: restored.digest }
  nativeError = Object.assign(new Error('private native text'), { code: 'REPLY_QUEUED' })
  await assert.rejects(adapter.answerContinuedQuestion(latest), { code: 'conflict', name: 'DefinitiveExecutionError' })
  nativeError = Object.assign(new Error('private native text'), { code: 'BAD_ANSWER' })
  await assert.rejects(adapter.answerContinuedQuestion(latest), { code: 'invalid_argument', name: 'DefinitiveExecutionError' })
  current = undefined
  assert.equal((await adapter.listQuestionPage('s')).batches[0].answerable, false)
})

test('channel lifecycle stops question projection before further native reads', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  let entered,release,sessionReads=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  const ctx={
    agents:{get:()=>undefined,roots:()=>[]},
    sessions:{get:()=>{sessionReads++;return null}},
    sessionController:{projections:async()=>{entered();await barrier;return {kind:'sequenced',asOfSeq:0,values:{userQuestions:{active:[],settled:[]}}}}},
  }
  const adapter=new RealDshAdapter({ctx})
  const abort=new AbortController()
  const pending=adapter.listQuestionPage('s',{signal:abort.signal})
  await started
  abort.abort('unavailable')
  release()
  await assert.rejects(pending,error=>error.code==='unavailable')
  assert.equal(sessionReads,0)
})

test('continued guards bind the exact live agent and session through admission', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  let session = {}, agent = { id: 's', session, inbox: { nextTurn: [], nextStep: [] } }
  let calls = 0, swapAfterSnapshot = false
  const ctx = {
    agents: { get: () => agent, roots: () => [agent] },
    sessions: { get: () => {
      const current = session
      if (swapAfterSnapshot) {
        swapAfterSnapshot = false
        queueMicrotask(() => { session = {}; agent = { id: 's', session, inbox: { nextTurn: [], nextStep: [] } } })
      }
      return current
    } },
    userQuestions: { answer: () => { calls++; return true } },
    sessionController: { projections: async () => ({ kind: 'sequenced', values: { userQuestions: {
      active: [{ callId: 'call', state: 'continued', questions }],
    } } }) },
  }
  const adapter = new RealDshAdapter({ ctx })
  const guard = batch => ({ session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] })
  const first = (await adapter.listProjectedQuestions('s'))[0]
  const same = (await adapter.listProjectedQuestions('s'))[0]
  assert.deepEqual(first.digest, same.digest)
  session = {}; agent = { id: 's', session, inbox: { nextTurn: [], nextStep: [] } }
  const second = (await adapter.listProjectedQuestions('s'))[0]
  assert.notDeepEqual(first.digest, second.digest)
  await assert.rejects(adapter.answerContinuedQuestion(guard(first)), { code: 'conflict' })
  assert.equal(calls, 0)
  swapAfterSnapshot = true
  await assert.rejects(adapter.answerContinuedQuestion(guard(second)), { code: 'conflict' })
  assert.equal(calls, 0)
  const third = (await adapter.listProjectedQuestions('s'))[0]
  assert.notDeepEqual(second.digest, third.digest)
  assert.equal((await adapter.answerContinuedQuestion(guard(third))).phase, 'QUESTION_REPLY_QUEUED')
  assert.equal(calls, 1)
  session = {}; agent.session = session
  const fourth = (await adapter.listProjectedQuestions('s'))[0]
  assert.notDeepEqual(third.digest, fourth.digest)
  await assert.rejects(adapter.answerContinuedQuestion(guard(third)), { code: 'conflict' })
  assert.equal(calls, 1)
})

test('open claims cannot settle after native agent or session replacement', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  for (const replaceAgent of [true, false]) {
    const agent = { id: 's', session: {} }, gate = Promise.withResolvers(), local = Promise.withResolvers()
    let current = agent, claims = 0
    const agents = { get: () => current, roots: () => [current] }
    const userQuestions = { async *attachWait() {
      claims++
      try { await gate.promise; yield { remainingMs: 1000 } } finally { claims-- }
    } }
    const handles = new QuestionHandles(), broker = new OpenQuestionBroker({ agents, userQuestions, handles })
    const native = broker.capture({ agent, questions, wait: { callId: 'call', timed: true } }, () => local.promise)
    const batch = broker.list('s')[0]
    const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
      expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
      answers: [{ id: 'q', selected: ['A'] }] }
    const attempt = broker.answer(request)
    await Promise.resolve()
    if (replaceAgent) current = { id: 's', session: {} }
    else agent.session = {}
    gate.resolve()
    await assert.rejects(attempt, { code: 'conflict' })
    assert.equal(claims, 0)
    assert.deepEqual(handles.listActive('s'), [])
    local.resolve({ answers: [{ id: 'q', selected: ['B'] }] })
    assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['B'] }] })
  }
})

test('interrupt during an open answer releases the claim so a later answer is not permanently conflicted', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const agent = { id: 's' }, agents = { get: () => agent, roots: () => [agent] }
  const handles = new QuestionHandles(), local = Promise.withResolvers(), gate = Promise.withResolvers()
  let claims = 0, entered = false
  const userQuestions = { async *attachWait(_agent, _call, signal) {
    claims++
    try {
      // Simulate a native claim acquisition that is still opening when the user
      // interrupts the session.
      entered = true
      await Promise.race([gate.promise, new Promise(resolve => signal.addEventListener('abort', resolve, { once: true }))])
      if (signal.aborted) return
      yield { remainingMs: 1000 }
    } finally { claims-- }
  } }
  const broker = new OpenQuestionBroker({ agents, userQuestions, handles })
  const native = broker.capture({ agent, questions, wait: { callId: 'call', timed: true } }, () => local.promise)
  const batch = broker.list('s')[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  const first = broker.answer(request)
  while (!entered) await new Promise(resolve => setImmediate(resolve))
  assert.equal(claims, 1)
  broker.interruptSession('s')
  await assert.rejects(first, error => error instanceof Error && error.code === 'conflict' && error.name === 'DefinitiveExecutionError')
  assert.equal(claims, 0)
  // No claim handle leaked and the same batch remains listed for a later answer.
  assert.deepEqual(handles.listActive('s'), [])
  assert.equal(broker.list('s').length, 1)
  gate.resolve()
  assert.equal((await broker.answer(request)).phase, 'QUESTION_SETTLED')
  assert.deepEqual(await native, { answers: [{ id: 'q', selected: ['A'] }] })
  local.resolve({ answers: [{ id: 'q', selected: ['B'] }] })
})

test('a session interrupt does not touch another session open answer', async () => {
  const { OpenQuestionBroker } = await import('../src/adapter/questions.js')
  const one = { id: 'one' }, two = { id: 'two' }
  const agents = { get: id => id === 'one' ? one : id === 'two' ? two : undefined, roots: () => [one, two] }
  const handles = new QuestionHandles(), local = Promise.withResolvers(), gate = Promise.withResolvers()
  let claims = 0, entered = false
  const userQuestions = { async *attachWait(_agent, _call, signal) {
    claims++
    try {
      entered = true
      await Promise.race([gate.promise, new Promise(resolve => signal.addEventListener('abort', resolve, { once: true }))])
      if (signal.aborted) return
      yield { remainingMs: 1000 }
    } finally { claims-- }
  } }
  const broker = new OpenQuestionBroker({ agents, userQuestions, handles })
  const pending = broker.capture({ agent: two, questions, wait: { callId: 'call', timed: true } }, () => local.promise)
  const batch = broker.list('two')[0]
  const attempt = broker.answer({ session_id: 'two', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] })
  while (!entered) await new Promise(resolve => setImmediate(resolve))
  broker.interruptSession('other')
  assert.equal(claims, 1)
  gate.resolve()
  // The other session's answer still settles normally and the question stays listed.
  assert.equal((await attempt).phase, 'QUESTION_SETTLED')
  assert.deepEqual(await pending, { answers: [{ id: 'q', selected: ['A'] }] })
  assert.equal(claims, 0)
  local.resolve({ answers: [{ id: 'q', selected: ['B'] }] })
})

test('fake session interrupt cancels a held open answer and keeps the question answerable', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const adapter = new FakeDshAdapter({ behavior: { questions: true, questionsOpen: true }, sessions: [{ sessionId: 's' }] })
  adapter.setFixtureQuestion('s', { callId: 'call', state: 'open', questions })
  adapter.holdFixtureQuestion('s')
  const batch = (await adapter.listQuestionPage('s')).batches[0]
  const request = { session_id: 's', batch_id: batch.batch_id, source_epoch: batch.source_epoch,
    expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
    answers: [{ id: 'q', selected: ['A'] }] }
  const pending = adapter.answerOpenQuestion(request)
  while (!adapter.fixtureQuestionWaiting('s')) await new Promise(resolve => setImmediate(resolve))
  await adapter.interrupt('s')
  await assert.rejects(pending, { code: 'unavailable' })
  assert.equal(adapter.fixtureQuestionWaiting('s'), false)
  assert.equal((await adapter.listQuestionPage('s')).batches.length, 1)
  assert.equal(adapter.describe().capabilities.includes('questions-answer-open'), true)
})

test('question read distinguishes an offline source from an unsupported read', async () => {
  const { RealDshAdapter } = await import('../src/adapter/real.js')
  const offline = new RealDshAdapter({ ctx: { agents: { get: () => undefined, roots: () => [] }, sessionController: {
    projections: async () => { throw Object.assign(new Error('offline'), { code: 'unavailable' }) },
  } } })
  await assert.rejects(offline.listProjectedQuestions('s'), { code: 'unavailable' })
  const unsupported = new RealDshAdapter({ ctx: {} })
  await assert.rejects(unsupported.listProjectedQuestions('s'), { code: 'unsupported_capability' })
})

test('fake question phases expose only answerable phases as answerable', async () => {
  const { FakeDshAdapter } = await import('../src/adapter/fake.js')
  const adapter = new FakeDshAdapter({ behavior: { questions: true, questionsOpen: true }, sessions: [{ sessionId: 's' }] })
  for (const [state, phase, answerable] of [
    ['open', 'QUESTION_OPEN', true],
    ['continued', 'QUESTION_CONTINUED', true],
    ['reply_queued', 'QUESTION_REPLY_QUEUED', false],
  ]) {
    adapter.setFixtureQuestion('s', { callId: `call-${state}`, state, questions })
    const batch = (await adapter.listQuestionPage('s')).batches[0]
    assert.equal(batch.phase, phase)
    assert.equal(batch.answerable, answerable)
  }
})
