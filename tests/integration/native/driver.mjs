// Test-only native setup and observations. Never part of the published connector.
import net from 'node:net'
import fs from 'node:fs'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { createHash } from 'node:crypto'
import { createRequire } from 'node:module'
import { codec } from '../../../host/runtime/src/protocol.js'
const installed = createRequire('/opt/native/node_modules/@deepseek-ai/dsh/package.json')
const { defineTool } = await import(installed.resolve('@deepseek-ai/dsh-tools'))
const { installModelSelection } = await import(installed.resolve('@deepseek-ai/dsh-agent'))
const { createUserMessage, createSystemMessage, createAssistantMessage, BlockAssembler } = await import(installed.resolve('@deepseek-ai/dsh-llm'))
const { RemoteError } = await import(installed.resolve('@deepseek-ai/dsh-typert-protocol'))
const { OpenQuestionBroker, QuestionHandles } = await import('/workspace/host/dsh/src/adapter/questions.js')
const { RealDshAdapter } = await import('/workspace/host/dsh/src/adapter/real.js')

// Test-only capability policy. The native adapter's projection, answer and
// lifecycle methods are inherited unchanged and never replaced at runtime.
class ContinuedFixtureAdapter extends RealDshAdapter {
  describe() {
    const source = super.describe()
    return { ...source, capabilities: [...source.capabilities, 'questions-read', 'questions-answer-continued'] }
  }
}


export const name = 'whalerein-native-driver'
export const inject = ['sessionController', 'sessions', 'settings', 'pluginManager', 'tools', 'approval', 'workspaceController', 'workspaceRegistry', 'userQuestions', 'agents', 'agentDefaultModel', 'llm']

export function apply(ctx) {
  const socketPath = process.env.WHALEREIN_NATIVE_SOCKET
  ctx.effect(() => {
    try { fs.unlinkSync(socketPath) } catch {}
    const cuts = new Map()
    const questionRuns = new Map()
    const staleQuestions = new Map()
    const managedRoots = new Map()
    let restoreAttachWait = null
    let heldQuestionClaims = 0
    let effects = 0
    let watchBarrier = null
    let gxOriginalSessionList = null
    let creationHistoryFault = null
    let gxHeldPairCommitResult = null
    let gxOriginal = null
    let gxWaitActive = 0
    let gxNativeWaitTimeouts = 0
    let gxDropped = 0
    let gxHeldCreate = null
    let gxHeldInterrupt = null
    let gxHeldNativeOperation = null
    let gxModelCounterArmed = false
    let modelReadCount = 0
    let modelSelectCount = 0
    let modelCatalogCount = 0
    let modelOwnerStream = null
    let modelOwnerAbort = null
    let gxQueueCounterArmed = false
    let gxQuestionCounterArmed = false
    let gxInterruptCounterArmed = false
    let gxForkCounterArmed = false
    let gxHeldQuestion = null
    let gxHeldContinued = null
    let gxStaleWait = null
    let waitRemoteFixture = null
    let channelQueueMutationCounter
    let replayCommitCounter
    let channelQueueReadCounter
let channelQueueBarrier = null
    let deliveryBarrier = null
    let deliveryPromptReplacement = null
    let deliveryServiceReplacement = null
    let deliveryManifestMutation = null
    let abortSteerObservation = null
    let questionResolve = null
    let questionState = 'none'
    const removeQuestion = ctx.on('user-questions/request', (request, next) => {
      if (request.questions?.[0]?.id !== 'model-fixture-question') return next()
      questionState = 'pending'
      return new Promise(resolve => { questionResolve = resolve })
    }, { global: true, prepend: true })
    const removeTool = ctx.tools.register(defineTool({
      name: 'native_fixture', description: 'Run the isolated approval fixture.', parameters: {},
      output: { schema: { type: 'integer' }, render: (_args, value) => [{ type: 'text', text: String(value) }] },
      async execute(_args, execution) {
        const decision = await ctx.approval.request({ agent: execution.agent, toolName: 'native_fixture',
          callId: execution.callId, reason: 'Native integration fixture', signal: execution.signal })
        if (decision === 'allowed-once' && !execution.signal.aborted) effects += 1
        return effects
      },
    }))
    // Deterministic native tools for parallel, failure and result-attachment shapes.
    const FIXTURE_PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGP4z8AARAwQCgAf7gP9i18U1AAAAABJRU5ErkJggg==', 'base64')
    const FIXTURE_FILE = Buffer.from('whalerein native fixture file bytes', 'utf8')
    const removeParallelA = ctx.tools.register(defineTool({
      name: 'native_fixture_parallel_a', description: 'Slow isolated parallel fixture.', parameters: {}, isConcurrencySafe: () => true,
      output: { schema: { type: 'integer' }, render: (_args, value) => [{ type: 'text', text: String(value) }] },
      async execute() { const started = Date.now(); await new Promise(resolve => setTimeout(resolve, 600)); toolRuns.push({ name: 'parallel_a', started, finished: Date.now() }); return 11 },
    }))
    const removeParallelB = ctx.tools.register(defineTool({
      name: 'native_fixture_parallel_b', description: 'Failing isolated parallel fixture.', parameters: {}, isConcurrencySafe: () => true,
      output: { schema: { type: 'integer' }, render: (_args, value) => [{ type: 'text', text: String(value) }] },
      async execute() { const started = Date.now(); toolRuns.push({ name: 'parallel_b', started, finished: Date.now() }); throw new Error('native fixture failure') },
    }))
    const removeBulk = ctx.tools.register(defineTool({
      name: 'native_fixture_bulk', description: 'Immediate concurrency-safe bulk fixture.', parameters: {}, isConcurrencySafe: () => true,
      output: { schema: { type: 'integer' }, render: (_args, value) => [{ type: 'text', text: String(value) }] },
      async execute() { return 1 },
    }))
    const removeAttachment = ctx.tools.register(defineTool({
      name: 'native_fixture_attachment', description: 'Stored image and exact file result fixture.', parameters: {},
      output: { schema: { type: 'object', properties: { image: { type: 'object', additionalProperties: true }, file: { type: 'object', additionalProperties: true } }, additionalProperties: true },
        render: (_args, value) => [{ type: 'image', attachment: value.image }, { type: 'file', attachment: value.file }] },
      async execute() {
        const attachments = ctx.get('attachments')
        const image = await attachments.saveImage({ data: FIXTURE_PNG, mediaType: 'image/png', name: 'native-fixture.png' })
        const file = await attachments.saveFile({ data: FIXTURE_FILE, name: 'native-fixture.bin' })
        return { image, file }
      },
    }))
    const host = () => ctx.get('whalereinSettings')?.host
    // Structural probe helpers export event and field shapes only. Message
    // bodies, tool arguments and attachment names never leave the fixture.
    const sha = value => createHash('sha256').update(typeof value === 'string' ? value : Buffer.from(value)).digest('hex')
    const keysOf = value => value && typeof value === 'object' && !Array.isArray(value) ? Object.keys(value).sort() : []
    const streamFrames = []
    const toolRuns = []
    const blockShape = block => {
      const type = block?.type
      const shape = { type, keys: keysOf(block) }
      if (type === 'text' || type === 'reasoning') {
        shape.textLen = typeof block.text === 'string' ? block.text.length : null
        shape.textDigest = typeof block.text === 'string' ? sha(block.text) : null
      } else if (type === 'tool-call') {
        shape.callDigest = sha(String(block.id ?? ''))
        shape.name = block.name
        shape.argsLen = typeof block.arguments === 'string' ? block.arguments.length : 0
      } else if (type === 'image' || type === 'file') {
        const attachment = block.attachment ?? {}
        shape.mediaType = attachment.mediaType ?? null
        shape.bytes = attachment.bytes ?? null
        shape.width = attachment.width ?? null
        shape.height = attachment.height ?? null
        shape.namePresent = typeof attachment.name === 'string' && attachment.name.length > 0
        shape.attachmentKeys = keysOf(attachment)
      }
      return shape
    }
    const eventShape = event => {
      const data = event?.data ?? {}
      const shape = { type: event?.type, seq: event?.seq, turn: data.turn ?? null, step: data.step ?? null, dataKeys: keysOf(data),
        surface: event?.surfaceOp === 'append' ? 'append' : event?.surfaceOp?.op ?? null,
        sourceEventSeqCount: Array.isArray(event?.sourceEventSeqs) ? event.sourceEventSeqs.length : 0 }
      if (event?.type === 'user/message') {
        shape.messageKeys = keysOf(data)
        shape.sourceKind = data.source?.kind ?? null
        shape.sourceForm = data.source?.form ?? null
        shape.messageIdDigest = sha(String(data.id ?? ''))
        shape.rpcIdPresent = typeof data.source?.rpcId === 'string'
        shape.rpcIdDigest = typeof data.source?.rpcId === 'string' ? sha(data.source.rpcId) : null
        shape.blocks = (data.content ?? []).map(blockShape)
      } else if (event?.type === 'system/message' || event?.type === 'developer/message') {
        shape.messageKeys = keysOf(data.message)
        shape.sourceKind = data.message?.source?.kind ?? null
        shape.sourceForm = data.message?.source?.form ?? null
        shape.blocks = (data.message?.content ?? []).map(blockShape)
      } else if (event?.type === 'assistant/message') {
        shape.messageKeys = keysOf(data.message)
        shape.messageIdDigest = sha(String(data.message?.id ?? ''))
        shape.provider = data.message?.source?.provider ?? null
        shape.model = data.message?.source?.model ?? null
        shape.blocks = (data.message?.content ?? []).map(blockShape)
        shape.interrupted = data.interrupted === true
        shape.streamTypes = [...new Set((data.stream ?? []).map(record => record?.type))]
        shape.usageKeys = keysOf(data.usage)
      } else if (event?.type === 'assistant/attempt') {
        shape.streamTypes = [...new Set((data.stream ?? []).map(record => record?.type))]
        shape.turn = data.turn
        shape.step = data.step
      } else if (event?.type === 'tool/call') {
        shape.callDigest = sha(String(data.callId ?? ''))
        shape.name = data.name
        shape.argsLen = typeof data.arguments === 'string' ? data.arguments.length : 0
      } else if (event?.type === 'tool/result') {
        shape.callDigest = sha(String(data.message?.toolCallId ?? ''))
        shape.isError = data.message?.isError === true
        shape.errorCode = data.error?.code ?? null
        shape.blocks = (data.message?.content ?? []).map(blockShape)
        shape.metaKeys = keysOf(data.meta)
      } else if (event?.type === 'turn/end') {
        shape.reasonKind = data.reason?.kind ?? null
      } else if (event?.type === 'request/header') {
        shape.reason = data.reason ?? null
        shape.configKeys = keysOf(data.header?.config)
        shape.provider = data.header?.config?.provider ?? null
        shape.model = data.header?.config?.model ?? null
        shape.thinking = data.header?.config?.thinking ?? null
        shape.reasoningEffort = data.header?.config?.reasoningEffort ?? null
        shape.maxTokens = data.header?.config?.maxTokens ?? null
      } else if (event?.type === 'llm/retry') {
        shape.retry = data.retry ?? null
        shape.maxRetries = data.maxRetries ?? null
        shape.delayMs = data.delayMs ?? null
        shape.mode = data.mode ?? null
        shape.failureCode = data.failure?.code ?? null
      } else if (event?.type === 'llm/retry-started') {
        shape.retry = data.retry ?? null
      } else if (event?.type === 'command/run') {
        shape.name = data.name ?? null
        shape.argsLen = typeof data.args === 'string' ? data.args.length : 0
        shape.sourceKind = data.source?.kind ?? null
      } else if (event?.type === 'command/done') {
        shape.kind = data.kind ?? null
        shape.textLen = typeof data.text === 'string' ? data.text.length : 0
      } else if (event?.type === 'compaction/start') {
        shape.turn = data.turn ?? null
        shape.commandPresent = typeof data.sourceCommandId === 'string'
      } else if (event?.type === 'compaction/summary') {
        shape.blockTypes = (data.summary ?? []).map(block => block?.type)
        shape.shadowedCount = Array.isArray(data.shadowedSeqs) ? data.shadowedSeqs.length : null
        shape.summaryPresent = Array.isArray(data.summary) && data.summary.length > 0
      } else if (event?.type === 'compaction/end') {
        shape.errorPresent = typeof data.error === 'string'
      } else if (event?.type === 'tool/ptc-dispatch' || event?.type === 'tool/ptc-dispatch-start') {
        shape.callDigest = sha(String(data.subCallId ?? data.callId ?? ''))
        shape.name = data.name ?? null
      }
      return shape
    }
    const occurrenceFrom = (events, wanted) => {
      for (let index = events.length - 1; index >= 0; index--) {
        const event = events[index]
        const content = event?.type === 'user/message' ? event.data?.content
          : event?.type === 'tool/result' ? event.data?.message?.content : null
        if (!Array.isArray(content)) continue
        for (let position = content.length - 1; position >= 0; position--) {
          const block = content[position]
          if (block?.type === wanted && block.attachment) return { ref: block.attachment, eventType: event.type, seq: event.seq }
        }
      }
      return null
    }
    const attachmentKeys = ref => ({ mediaType: ref?.mediaType ?? null, bytes: ref?.bytes ?? null,
      width: ref?.width ?? null, height: ref?.height ?? null,
      namePresent: typeof ref?.name === 'string' && ref.name.length > 0, keys: keysOf(ref) })
    const readFileAll = async (ref, signal) => {
      const chunks = []
      for await (const chunk of ctx.get('attachments').readFileStream(ref, signal)) chunks.push(Buffer.from(chunk))
      const data = Buffer.concat(chunks)
      return { chunks: chunks.length, bytes: data.length, digest: sha(data) }
    }
    const coldOccurrence = async (sessionId, wanted, limit = 200) => {
      let beforeSeq
      let pages = 0, records = 0, bytes = 0
      for (let page = 0; page < limit; page++) {
        const value = await ctx.sessionController.events({ address: { kind: 'session', sessionId },
          ...(beforeSeq === undefined ? {} : { beforeSeq }), maxMessages: 200 }, AbortSignal.timeout(20000))
        pages += 1
        records += value.records.length
        bytes += JSON.stringify(value.records).length
        const events = value.records.map(record => record.event)
        const found = occurrenceFrom(events, wanted)
        if (found) return { ...found, source: 'cold', pages, records, bytes }
        if (!value.hasMore || !events.length) break
        beforeSeq = events[0].seq
      }
      return { source: 'cold', missing: true, pages, records, bytes }
    }
    const historyEvents = async (sessionId, options = {}) => {
      const live = ctx.sessions.get(sessionId)
      if (live && typeof live.snapshotEvents === 'function') {
        return { source: 'hot', events: live.snapshotEvents() }
      }
      const page = await ctx.sessionController.events({ address: { kind: 'session', sessionId },
        maxMessages: options.maxMessages ?? 500 }, AbortSignal.timeout(20000))
      return { source: 'cold', events: page.records.map(record => record.event) }
    }
    ctx.on('agent/assistant-stream', notification => {
      const sessionId = notification?.agent?.session?.id
      const frame = notification?.frame
      if (typeof sessionId !== 'string' || !frame) return
      const entry = { sessionId, phase: frame.type, attemptDigest: sha(String(frame.attemptId ?? '')),
        revision: frame.revision ?? null, turn: frame.turn ?? null, step: frame.step ?? null }
      if (frame.type === 'chunk') {
        entry.recordIndex = frame.index ?? null
        entry.chunkType = frame.chunk?.type ?? null
        entry.blockType = frame.chunk?.blockType ?? frame.chunk?.block?.type ?? null
        entry.chunkIndex = frame.chunk?.index ?? null
      } else if (frame.type === 'end') {
        entry.outcomeKind = frame.outcome?.kind ?? null
        entry.outcomeEventType = frame.outcome?.eventType ?? null
      }
      streamFrames.push(entry)
      if (streamFrames.length > 4000) streamFrames.splice(0, streamFrames.length - 4000)
    }, { global: true })
    const server = net.createServer(socket => {
      let buffer = ''
      socket.on('data', chunk => {
        buffer += chunk
        if (buffer.length > 65536) { socket.destroy(); return }
        if (!buffer.includes('\n')) return
        const line = buffer.slice(0, buffer.indexOf('\n'))
        buffer = ''
        Promise.resolve().then(async () => {
          const { method, sessionId, itemId, deviceId, config, selection, otherSelection, timeoutMs, count, cold, beforeSeq, throughSeq, maxMessages, maxPages, cancel, occurrenceKind, fromSeq, toSeq, bytes, turns, turnOffset } = JSON.parse(line)
          const signal = AbortSignal.timeout(15000)
          if (method === 'cycle-controller') {
            const raw = ctx.sessionController[Symbol.for('cordis.original')]
            const fiber = raw.ctx.fiber
            const parent = fiber.parent
            const Controller = fiber.runtime.callback
            const config = fiber.config
            globalThis.__whalereinCycle = {raw, host: host()}
            const resultFile = socketPath + '.controller-cycle'
            setTimeout(async () => {
              try {
                await fiber.dispose()
                fs.writeFileSync(resultFile, 'removed')
                await parent.plugin(Controller, config)
                fs.writeFileSync(resultFile, 'recreated')
              } catch {
                fs.writeFileSync(resultFile, 'failed')
              }
            }, 100)
            return {armed:true}
          }
          if (method === 'cycle-status') {
            const old = globalThis.__whalereinCycle
            return { newController: Boolean(old && ctx.sessionController[Symbol.for('cordis.original')] !== old.raw),
              newHost: Boolean(old?.host && host() && host() !== old.host) }
          }
          if (method === 'booted') return { ready: true }
          if (method === 'effects') return { count: effects }
          if (method === 'install') {
            const result = await ctx.pluginManager.installBundle('/workspace/host/dsh')
            return { application: result.application, warnings: result.warnings?.length ?? 0 }
          }
          if (method === 'seed') {
            await ctx.settings.update('whalerein-dsh', config)
            return { seeded: true }
          }
          if (method === 'connection-settings') {
            const row = ctx.settings.describe({redactSecrets: true}).find((value) => value.ns === 'whalerein-dsh')
            const service = row?.user?.service
            return {service: ['whalerein_cloud', 'custom', 'standalone'].includes(service) ? service : null}
          }
          if (method === 'ready') {
            const status = ctx.get('whalereinSettings')?.snapshot()
            return { available: status?.available === true, connected: status?.connected === true,
              source: status?.diagnostics?.dshAvailable === true,
              replayPending: status?.diagnostics?.revocationsPending !== false }
          }
          if (method === 'workspace') {
            await ctx.workspaceController.create({ path: process.cwd() })
            return { ready: true }
          }
          if (method === 'session-count') return { count: (await ctx.sessionController.list({}, signal)).items.length }
          if (method === 'mark-working-directory') {
            cuts.set('fixture-workdir', fs.readdirSync(process.cwd()).sort())
            return { marked: true }
          }
          if (method === 'working-directory-unchanged') {
            return { unchanged: JSON.stringify(cuts.get('fixture-workdir')) === JSON.stringify(fs.readdirSync(process.cwd()).sort()) }
          }
          if (method === 'test-enable-fork') {
            const adapter = host().adapter
            const describe = adapter.describe.bind(adapter)
            const describeHost = adapter.describeHostCapabilities.bind(adapter)
            adapter.describe = () => { const result = describe(); return {...result, capabilities:[...new Set([...(result.capabilities ?? []), 'session-fork'])]} }
            adapter.describeHostCapabilities = async () => ({...(await describeHost()), session_fork:true})
            return {enabled:true}
          }
          if (method === 'arm-fork-attach-fault') {
            const result = await ctx.workspaceController.create({path:process.cwd()})
            const workspace = ctx.workspaceRegistry.get(result.workspace.workspaceId)
            await workspace.attachSession(sessionId)
            const attach = workspace.attachSession
            workspace.attachSession = async (...args) => { workspace.attachSession = attach; throw new Error('injected fixture fault') }
            return {armed:true}
          }
          if (method === 'arm-fork-preset-spoof') {
            const presets = ctx.get('agentPresets')
            if (!presets) throw new Error('fixture presets unavailable')
            const resolve = presets.resolve
            presets.resolve = async function(...args) {
              presets.resolve = resolve
              throw new RemoteError('session/workspace-attach-failed', 'synthetic nested fault', {sessionId:config.victimId})
            }
            return {armed:true}
          }
          if (method === 'arm-fork-postchange-fault') {
            const adapter = host().adapter
            const fork = adapter.forkSession
            adapter.forkSession = async function(...args) {
              adapter.forkSession = fork
              await fork.apply(adapter, args)
              throw new Error('injected post-change fault')
            }
            return {armed:true}
          }
          if (method === 'arm-fork-journal-fault') {
            const store = host().store
            const update = store.setCommandStatus
            store.setCommandStatus = function(fields) {
              if (fields.status === 'succeeded') { store.setCommandStatus = update; throw new Error('injected fixture fault') }
              return update.call(store, fields)
            }
            return {armed:true}
          }
          if (method === 'fork-with-attach-fault') {
            const result = await ctx.workspaceController.create({path: process.cwd()})
            const workspace = ctx.workspaceRegistry.get(result.workspace.workspaceId)
            if (!workspace) throw new Error('fixture workspace unavailable')
            await workspace.attachSession(sessionId)
            const attach = workspace.attachSession
            workspace.attachSession = async () => { throw new Error('injected fixture fault') }
            try { const facts = await host().adapter.forkSession(sessionId); facts.rejected = !facts?.identity; return facts }
            catch (error) { return {rejected:true, code:error.code, definitive:error.name === 'DefinitiveExecutionError'} }
            finally { workspace.attachSession = attach }
          }
          if (method === 'fork-empty-check') {
            const result = await host().adapter.forkSession(sessionId)
            if (result?.identity) return { rejected: false }
            return { rejected: true, code: result?.failure_code ?? 'unknown', definitive: result?.entered === false }
          }
          if (method === 'snapshot-prefix-digest') {
            const snapshot = await host().adapter.getSessionSnapshot(sessionId)
            const events = snapshot.events.slice(0, config?.count ?? snapshot.events.length).map(event => ({type:event.type,data:event.data}))
            return { count: events.length, contextCount: events.filter(event => ['system/message','developer/message'].includes(event.type)).length, digest: createHash('sha256').update(JSON.stringify(events)).digest('hex') }
          }
          if (method === 'snapshot-content-digest') {
            const snapshot = await host().adapter.getSessionSnapshot(sessionId)
            const messages = snapshot.events.filter(event => ['user/message','assistant/message','tool/result'].includes(event.type)).map(event => ({type:event.type,data:event.data}))
            return { count: messages.length, digest: createHash('sha256').update(JSON.stringify(messages)).digest('hex') }
          }
          if (method === 'fork-native-default-probe') {
            const defaults = ctx.get('agentDefaultModel')
            const agents = ctx.get('agents')
            const presets = ctx.get('agentPresets')
            const parent = agents?.get(sessionId)
            if (!defaults || !parent || !presets) throw new Error('fixture native services unavailable')
            const previous = JSON.stringify(parent.model.current())
            const preset = presets.presetForSession(parent.session)
            const original = defaults.currentSelection
            defaults.currentSelection = () => ({provider:'fixture-default-provider',model:'fixture-default-model'})
            try {
              const facts = await host().adapter.forkSession(sessionId)
              const receipt = { session_id: facts.identity?.session_id ?? '' }
              const child = agents.get(receipt.session_id)
              const selected = child?.model?.current()
              return { childId:receipt.session_id, defaultFallback:child?.model?.options?.provider === 'fixture-default-provider' && child?.model?.options?.model === 'fixture-default-model', currentInherited:JSON.stringify(selected) === previous, parentUnchanged:JSON.stringify(parent.model.current()) === previous, inheritedPreset:presets.presetForSession(child.session) === preset }
            } finally { defaults.currentSelection = original }
          }
          if (method === 'gx-user-delivery-count') {
            const snapshot = await host().adapter.getSessionSnapshot(sessionId)
            return { count:snapshot.events.filter(event => event.type === 'user/message' && JSON.stringify(event.data).includes('synthetic delivery')).length }
          }
          if (method === 'synthetic-tool-marker-present') {
            const snapshot = await host().adapter.getSessionSnapshot(sessionId)
            return {present: snapshot.events.some(event => JSON.stringify(event).includes('fork-synthetic-tool-parameter-b4821'))}
          }
          if (method === 'snapshot-types') {
            const snapshot = await host().adapter.getSessionSnapshot(sessionId)
            return { types: snapshot.events.map(event => event.type) }
          }
          if (method === 'same-cwd') {
            const rows = (await ctx.sessionController.list({}, signal)).items
            const left = rows.find(row => row.sessionId === sessionId)
            const right = rows.find(row => row.sessionId === config.childId)
            return { same: !!left && !!right && left.cwd === right.cwd && typeof left.cwd === 'string' }
          }
          if (method === 'fork-adapter') return host().adapter.forkSession(sessionId)
          if (method === 'test-model-owner-open') {
            const gateway = ctx.get('typertGateway')
            modelOwnerAbort = new AbortController()
            const peer = {principal:{kind:'owner'}, signal:modelOwnerAbort.signal}
            modelOwnerStream = gateway.openRemoteEvents({args:{}}, modelOwnerAbort.signal, peer)
            const first = await modelOwnerStream.next()
            return {ready:first.value?.type === 'ready', live:gateway.hasLiveClient()}
          }
          if (method === 'test-model-owner-close') {
            modelOwnerAbort?.abort()
            await modelOwnerStream?.return().catch(() => {})
            modelOwnerStream = null
            modelOwnerAbort = null
            return {closed:true}
          }
          if (method === 'test-model-gateway-question-start') {
            const agent = ctx.agents.get(sessionId)
            const pending = ctx.userQuestions.ask({agent, questions:[{id:'gateway-fixture', question:'Fixture'}]})
            pending.catch(() => {})
            questionRuns.set(sessionId, pending)
            await new Promise(resolve => setImmediate(resolve))
            return {started:true}
          }
          if (method === 'test-model-gateway-pending') {
            return {events:ctx.get('typertGateway').getPendingRemoteEvents()
              .filter(item => String(item.agentId) === sessionId).map(item => item.event)}
          }
          if (method === 'test-model-catalog-only') {
            const adapter = host().adapter
            const original = adapter.describe.bind(adapter)
            const originalRead = adapter.readSessionModel.bind(adapter)
            const originalSelect = adapter.selectSessionModel.bind(adapter)
            adapter.describe = () => ({...original(), capabilities: ['model-catalog']})
            adapter.describeHostCapabilities = async () => ({model_catalog: true, model_read: false})
            adapter.readSessionModel = async (...args) => { modelReadCount++; return originalRead(...args) }
            adapter.selectSessionModel = async (...args) => { modelSelectCount++; return originalSelect(...args) }
            return {enabled:true}
          }
          if (method === 'test-model-catalog-only-counts') return {read:modelReadCount, select:modelSelectCount}
          if (method === 'test-model-counts-enable') {
            const adapter = host().adapter
            const catalog = adapter.listModels.bind(adapter)
            const read = adapter.readSessionModel.bind(adapter)
            const select = adapter.selectSessionModel.bind(adapter)
            adapter.listModels = async (...args) => { modelCatalogCount++; return catalog(...args) }
            adapter.readSessionModel = async (...args) => { modelReadCount++; return read(...args) }
            adapter.selectSessionModel = async (...args) => { modelSelectCount++; return select(...args) }
            return {enabled:true}
          }
          if (method === 'test-model-counts') return {catalog:modelCatalogCount, read:modelReadCount, select:modelSelectCount}
          if (method === 'test-enable-gx') {
            const adapter = host().adapter
            const original = adapter.describe.bind(adapter)
            const hostCapabilities = adapter.describeHostCapabilities.bind(adapter)
            const listQuestions = adapter.listQuestionPage.bind(adapter)
            adapter.describe = () => { const value = original(); return {...value, capabilities:[...new Set([...(value.capabilities ?? []), 'model-catalog', 'model-read', 'model-select', 'session-fork', 'wait', 'questions-read', 'questions-answer-continued'])]} }
            // Test-only admission of the continued candidate; production remains closed.
            adapter.listQuestionPage = async (...args) => { const page = await listQuestions(...args); return {...page, batches:page.batches.map(batch => batch.phase === 'QUESTION_CONTINUED' ? {...batch,answerable:true} : batch)} }
            adapter.describeHostCapabilities = async () => ({...(await hostCapabilities()),session_create:true,session_fork:true,model_catalog:true,model_read:true})
            adapter.deliveryModes = ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER']
            // The production real adapter re-checks its verified runtime on
            // every call. This fixture-only override exercises the GX scenarios
            // without weakening that production gate.
            adapter.supportedDeliveryModes = () => adapter.deliveryModes
            return {enabled:true}
          }
          if (method === 'gx-hold-interrupt') {
            if (gxHeldInterrupt) throw new Error('interrupt already held')
            const adapter = host().adapter, original = adapter.interrupt, release = Promise.withResolvers()
            const state = {entered:false, release, adapter, original}
            gxHeldInterrupt = state
            adapter.interrupt = async function(...args) { state.entered = true; await release.promise; return original.apply(adapter, args) }
            return {armed:true}
          }
          if (method === 'gx-held-interrupt-status') return {entered:gxHeldInterrupt?.entered === true}
          if (method === 'gx-release-interrupt') {
            if (!gxHeldInterrupt) throw new Error('interrupt not held')
            const state = gxHeldInterrupt; gxHeldInterrupt = null
            state.adapter.interrupt = state.original
            state.release.resolve()
            return {released:true}
          }
          if (method === 'gx-arm-fork-counter') {
            if (gxForkCounterArmed) throw new Error('fork counter already armed')
            gxForkCounterArmed = true
            const controller = ctx.sessionController, original = controller.fork, file = socketPath + '.gx-fork-count'
            if (!fs.existsSync(file)) fs.writeFileSync(file, '0')
            controller.fork = async function(...args) {
              fs.writeFileSync(file, String(Number(fs.readFileSync(file, 'utf8')) + 1))
              return original.apply(controller,args)
            }
            return {armed:true}
          }
          if (method === 'gx-fork-count') {
            const file = socketPath + '.gx-fork-count'
            return {count:fs.existsSync(file) ? Number(fs.readFileSync(file, 'utf8')) : 0}
          }
          if (method === 'gx-arm-interrupt-counter') {
            if (gxInterruptCounterArmed) throw new Error('interrupt counter already armed')
            gxInterruptCounterArmed = true
            const controller = ctx.sessionController, original = controller.cancel, file = socketPath + '.gx-interrupt-count'
            if (!fs.existsSync(file)) fs.writeFileSync(file, '0')
            controller.cancel = function(...args) {
              fs.writeFileSync(file, String(Number(fs.readFileSync(file, 'utf8')) + 1))
              return original.apply(controller,args)
            }
            return {armed:true}
          }
          if (method === 'gx-interrupt-count') {
            const file = socketPath + '.gx-interrupt-count'
            return {count:fs.existsSync(file) ? Number(fs.readFileSync(file, 'utf8')) : 0}
          }
          if (method === 'gx-arm-question-counter') {
            if (gxQuestionCounterArmed) throw new Error('question counter already armed')
            const key = config?.phase === 'open' ? 'answerOpenQuestion' : config?.phase === 'continued' ? 'answerContinuedQuestion' : ''
            if (!key) throw new Error('unknown question phase')
            gxQuestionCounterArmed = true
            const adapter = host().adapter, original = adapter[key], file = socketPath + '.gx-question-count'
            if (!fs.existsSync(file)) fs.writeFileSync(file, '0')
            adapter[key] = async function(...args) {
              fs.writeFileSync(file, String(Number(fs.readFileSync(file, 'utf8')) + 1))
              return original.apply(adapter,args)
            }
            return {armed:true}
          }
          if (method === 'gx-question-count') {
            const file = socketPath + '.gx-question-count'
            return {count:fs.existsSync(file) ? Number(fs.readFileSync(file, 'utf8')) : 0}
          }
          if (method === 'gx-arm-queue-counter') {
            if (gxQueueCounterArmed) throw new Error('queue counter already armed')
            gxQueueCounterArmed = true
            const controller = ctx.sessionController, original = controller.updateQueue, file = socketPath + '.gx-queue-count'
            if (!fs.existsSync(file)) fs.writeFileSync(file, '0')
            controller.updateQueue = async function(...args) {
              fs.writeFileSync(file, String(Number(fs.readFileSync(file, 'utf8')) + 1))
              return original.apply(controller,args)
            }
            return {armed:true}
          }
          if (method === 'gx-queue-count') {
            const file = socketPath + '.gx-queue-count'
            return {count:fs.existsSync(file) ? Number(fs.readFileSync(file, 'utf8')) : 0}
          }
          if (method === 'gx-arm-model-counter') {
            if (gxModelCounterArmed) throw new Error('counter already armed')
            gxModelCounterArmed = true
            const adapter = host().adapter, original = adapter.selectSessionModel, file = socketPath + '.gx-model-count'
            if (!fs.existsSync(file)) fs.writeFileSync(file, '0')
            adapter.selectSessionModel = async function(...args) {
              fs.writeFileSync(file, String(Number(fs.readFileSync(file, 'utf8')) + 1))
              return original.apply(adapter,args)
            }
            return {armed:true}
          }
          if (method === 'gx-model-count') {
            const file = socketPath + '.gx-model-count'
            return {count:fs.existsSync(file) ? Number(fs.readFileSync(file, 'utf8')) : 0}
          }
          if (method === 'gx-arm-stale-wait') {
            if (gxStaleWait) throw new Error('stale wait already armed')
            const controller = ctx.sessionController, original = controller.wait, release = Promise.withResolvers()
            const state = {calls:0,entered:false,aborted:false,lateReturned:false,controller,original,release}
            gxStaleWait = state
            controller.wait = async function(request, nativeSignal) {
              state.calls++
              if (state.calls > 1) return original.call(controller,request,nativeSignal)
              state.channelId = host().security.listChannels().at(-1)?.channelId
              state.entered = true
              nativeSignal.addEventListener('abort', () => { state.aborted = true }, {once:true})
              if (nativeSignal.aborted) state.aborted = true
              await release.promise
              state.lateReturned = true
              return {reason:'idle',state:{active:true,running:false,queue:[],pendingInput:[]}}
            }
            return {armed:true}
          }
          if (method === 'gx-stale-wait-status') return {entered:gxStaleWait?.entered === true, aborted:gxStaleWait?.aborted === true, lateReturned:gxStaleWait?.lateReturned === true, calls:gxStaleWait?.calls ?? 0}
          if (method === 'gx-close-stale-wait-channel') {
            if (!gxStaleWait?.channelId) throw new Error('stale wait channel missing')
            host().security.closeChannel(gxStaleWait.channelId)
            return {closed:true}
          }
          if (method === 'gx-release-stale-wait') {
            if (!gxStaleWait) throw new Error('stale wait not armed')
            gxStaleWait.release.resolve()
            return {released:true}
          }
          if (method === 'gx-reset-stale-wait') {
            if (gxStaleWait) {
              const state = gxStaleWait; gxStaleWait = null
              state.release.resolve(); state.controller.wait = state.original
            }
            return {reset:true}
          }
          if (method === 'gx-hold-continued-answer') {
            if (gxHeldContinued) throw new Error('continued answer already held')
            const service = ctx.userQuestions, original = service.answer, release = Promise.withResolvers()
            const state = {entered:false,calls:0,service,original,release}
            gxHeldContinued = state
            service.answer = async function(...args) { state.entered = true; await release.promise; state.calls++; return original.apply(service,args) }
            return {armed:true}
          }
          if (method === 'gx-held-continued-status') return {entered:gxHeldContinued?.entered === true,calls:gxHeldContinued?.calls ?? 0}
          if (method === 'gx-release-continued-answer') {
            if (!gxHeldContinued) throw new Error('continued answer not held')
            gxHeldContinued.release.resolve()
            return {released:true}
          }
          if (method === 'gx-reset-continued-answer') {
            if (gxHeldContinued) {
              const state = gxHeldContinued; gxHeldContinued = null
              state.release.resolve(); state.service.answer = state.original
            }
            return {reset:true}
          }
          if (method === 'gx-hold-question-claim') {
            if (gxHeldQuestion) throw new Error('question claim already held')
            const service = ctx.userQuestions, original = service.attachWait, release = Promise.withResolvers()
            const state = {entered:false, service, original, release}
            gxHeldQuestion = state
            service.attachWait = (agent, callId, abortSignal) => (async function* () {
              const iterator = original.call(service,agent,callId,abortSignal)
              try {
                const opening = await iterator.next()
                if (opening.done) return
                state.entered = true
                await release.promise
                yield opening.value
                yield* iterator
              } finally { await iterator.return() }
            })()
            return {armed:true}
          }
          if (method === 'gx-held-question-status') return {entered:gxHeldQuestion?.entered === true}
          if (method === 'gx-release-question-claim') {
            if (!gxHeldQuestion) throw new Error('question claim not held')
            gxHeldQuestion.release.resolve()
            return {released:true}
          }
          if (method === 'gx-reset-question-claim') {
            if (gxHeldQuestion) {
              const state = gxHeldQuestion; gxHeldQuestion = null
              state.release.resolve(); state.service.attachWait = state.original
            }
            return {reset:true}
          }
          if (method === 'gx-hold-native-operation') {
            if (gxHeldNativeOperation) throw new Error('native operation already held')
            const operation = config?.operation
            const methods = {send:'prompt', steer:'prompt', model:'selectModel', fork:'fork', queue_edit:'updateQueue', queue_remove:'updateQueue', queue_promote:'updateQueue'}
            const key = methods[operation], controller = ctx.sessionController
            if (!key || typeof controller[key] !== 'function') throw new Error('unsupported fixture operation')
            const original = controller[key], release = Promise.withResolvers()
            const state = {entered:false, calls:0, released:false, controller, key, original, release}
            gxHeldNativeOperation = state
            controller[key] = async function(...args) { state.entered = true; await release.promise; state.calls++; return original.apply(controller,args) }
            return {armed:true}
          }
          if (method === 'gx-held-native-status') return {entered:gxHeldNativeOperation?.entered === true, calls:gxHeldNativeOperation?.calls ?? 0}
          if (method === 'gx-release-native-operation') {
            if (!gxHeldNativeOperation) throw new Error('native operation not held')
            gxHeldNativeOperation.released = true
            gxHeldNativeOperation.release.resolve()
            return {released:true}
          }
          if (method === 'gx-reset-native-operation') {
            if (gxHeldNativeOperation) {
              const state = gxHeldNativeOperation; gxHeldNativeOperation = null
              state.release.resolve(); state.controller[state.key] = state.original
            }
            return {reset:true}
          }
          if (method === 'gx-disable-one-capability') {
            const adapter = host().adapter, operation = config?.operation
            const names = {model:'model-select', fork:'session-fork', queue_edit:'pending-queue-edit', queue_remove:'pending-queue-remove', queue_promote:'pending-queue-steer', question_open:'questions-answer-open', question_continued:'questions-answer-continued'}
            if (operation === 'send') adapter.deliveryModes = ['START_IF_IDLE', 'STEER']
            else if (operation === 'steer') adapter.deliveryModes = ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN']
            else {
              const disabled = names[operation]
              if (!disabled) throw new Error('unsupported fixture capability')
              const describe = adapter.describe.bind(adapter)
              adapter.describe = () => { const value = describe(); return {...value, capabilities:(value.capabilities ?? []).filter(item => item !== disabled)} }
              if (operation === 'fork') {
                const hostCapabilities = adapter.describeHostCapabilities.bind(adapter)
                adapter.describeHostCapabilities = async () => ({...(await hostCapabilities()),session_fork:false})
              }
            }
            return {disabled:true}
          }
          if (method === 'gx-hold-create') {
            if (gxHeldCreate) throw new Error('create already held')
            const controller = ctx.sessionController, original = controller.create, release = Promise.withResolvers()
            const state = {entered:false, release, controller, original}
            gxHeldCreate = state
            controller.create = async function(...args) { state.entered = true; await release.promise; return original.apply(controller, args) }
            return {armed:true}
          }
          if (method === 'gx-held-create-status') return {entered:gxHeldCreate?.entered === true}
          if (method === 'gx-disable-create-capability') {
            const adapter = host().adapter, original = adapter.describeHostCapabilities.bind(adapter)
            adapter.describeHostCapabilities = async () => ({...(await original()),session_create:false})
            return {disabled:true}
          }
          if (method === 'gx-release-create') {
            if (!gxHeldCreate) throw new Error('create not held')
            const state = gxHeldCreate; gxHeldCreate = null
            state.controller.create = state.original
            state.release.resolve()
            return {released:true}
          }
          if (method === 'gx-drop-next-command-success') {
            const access = host().access, publish = access.publishResult, protocol = codec()
            access.publishResult = async function(bytes) {
              const result = protocol.decode('whalerein.v1.HostResult', bytes)
              if (result.command_id && result.status === 'succeeded') { access.publishResult = publish; gxDropped++; return }
              return publish.call(access, bytes)
            }
            return {armed:true}
          }
          if (method === 'gx-drop-status') return {count:gxDropped}
          if (method === 'gx-arm-wait-capability') {
            const adapter = host().adapter
            if (gxOriginal) throw new Error('GX wait already armed')
            gxOriginal = {describe:adapter.describe, waitSession:adapter.waitSession}
            const describe = adapter.describe.bind(adapter), waitSession = adapter.waitSession.bind(adapter)
            adapter.describe = () => { const value = describe(); return {...value, capabilities:[...new Set([...(value.capabilities ?? []), 'wait'])]} }
            adapter.waitSession = async (...args) => { gxWaitActive++; try { return await waitSession(...args) } finally { gxWaitActive-- } }
            return {enabled:true}
          }
          if (method === 'gx-wait-active') return {active:gxWaitActive}
          if (method === 'gx-arm-short-native-wait') {
            const adapter = host().adapter
            if (gxOriginal) throw new Error('GX wait already armed')
            gxOriginal = {describe:adapter.describe, waitSession:adapter.waitSession}
            const describe = adapter.describe.bind(adapter), waitSession = adapter.waitSession.bind(adapter)
            adapter.describe = () => { const value = describe(); return {...value, capabilities:[...new Set([...(value.capabilities ?? []), 'wait'])]} }
            adapter.waitSession = async (id, timeout, signal) => {
              gxWaitActive++
              try {
                const result = await waitSession(id, Math.min(timeout, 500), signal)
                if (result.reason === 'timeout') gxNativeWaitTimeouts++
                return result
              } finally { gxWaitActive-- }
            }
            return {enabled:true}
          }
          if (method === 'gx-native-wait-timeouts') return {count:gxNativeWaitTimeouts}
          if (method === 'gx-disable-wait-capability') {
            if (!gxOriginal) throw new Error('GX wait not armed')
            const adapter = host().adapter
            const originalDescribe = gxOriginal.describe.bind(adapter)
            adapter.describe = () => {
              const value = originalDescribe()
              return {...value, capabilities:(value.capabilities ?? []).filter(item => item !== 'wait')}
            }
            adapter.waitSession = async () => { throw new AdapterError('unsupported_capability') }
            gxOriginal = null
            return {disabled:true}
          }
          if (method === 'test-enable-create') {
            host().adapter.describeHostCapabilities = async () => ({session_create:true})
            return {enabled:true}
          }
          if (method === 'workspace-id') {
            const {workspace} = await ctx.workspaceController.create({path:process.cwd()})
            return {workspaceId:workspace.workspaceId}
          }
          if (method === 'test-fail-next-attachment') {
            const {workspace} = await ctx.workspaceController.create({path:process.cwd()})
            const entity = ctx.workspaceRegistry.get(workspace.workspaceId)
            const original = entity.attachSession
            entity.attachSession = async (...args) => { entity.attachSession = original; throw new Error('synthetic attachment failure') }
            return {armed:true}
          }
          if (method === 'workspace-has-session') {
            const {workspace} = await ctx.workspaceController.create({path:process.cwd()})
            return {attached:ctx.workspaceRegistry.get(workspace.workspaceId).sessionIds.includes(sessionId)}
          }
          if (method === 'workspace-native-create') {
            const {workspace} = await ctx.workspaceController.create({path:process.cwd()})
            const receipt = await host().adapter.executeHost({create_session:{workspace_id:workspace.workspaceId}})
            const sessionId = receipt.identity?.session_id ?? ''
            return {receipt, attached: Boolean(sessionId) && ctx.workspaceRegistry.get(workspace.workspaceId).sessionIds.includes(sessionId)}
          }
          if (method === 'workspace-attach-fail') {
            const {workspace} = await ctx.workspaceController.create({path:process.cwd()})
            const entity = ctx.workspaceRegistry.get(workspace.workspaceId)
            const original = entity.attachSession
            entity.attachSession = async () => { throw new Error('synthetic attachment fault') }
            try { return {result:await host().adapter.executeHost({create_session:{workspace_id:workspace.workspaceId, ...(sessionId ? {preferred_session_id:sessionId} : {})}})} }
            catch (error) { return {code:error.code, definitive:error.constructor.name === 'DefinitiveExecutionError'} }
            finally { entity.attachSession = original }
          }
          if (method === 'default-preset-info') {
            const presets = ctx.get('agentPresets')
            if (!presets) return {available:false}
            return {available:true, presetId:(await presets.resolve()).id}
          }
          if (method === 'session-has-default-preset') {
            const presets = ctx.get('agentPresets')
            return {matches:!!presets && presets.presetForSession(ctx.sessions.get(sessionId)) === (await presets.resolve()).id}
          }
          if (method === 'create-explicit-preset') {
            const presets = ctx.get('agentPresets')
            if (!presets) return {available:false}
            const preset = (await presets.resolve()).id
            const result = await host().adapter.executeHost({create_session:{agent_preset:preset}})
            const sessionId = result.identity?.session_id ?? ''
            return {available:true, confirmed:Boolean(sessionId) && preset === presets.presetForSession(ctx.sessions.get(sessionId)), receipt:result}
          }
          if (method === 'create-missing-preset') {
            try { return {result:await host().adapter.executeHost({create_session:{agent_preset:'fixture-nonexistent-preset'}})} }
            catch (error) { return {code:error.code, definitive:error.constructor.name === 'DefinitiveExecutionError'} }
          }
          if (method === 'archive-session') {
            const result = await ctx.workspaceController.archiveSession({sessionId})
            return {archived:result.archivedSessionIds.includes(sessionId)}
          }
          if (method === 'create-preferred-race') {
            return await Promise.all([1, 2].map(() => host().adapter.executeHost({create_session:{preferred_session_id:sessionId}})))
          }
          if (method === 'create-missing-workspace') {
            try { return {result:await host().adapter.executeHost({create_session:{workspace_id:'fixture-no-such-workspace'}})} }
            catch (error) { return {code:error.code, definitive:error.constructor.name === 'DefinitiveExecutionError'} }
          }
          if (method === 'create-explicit-directory') {
            const cwd = process.cwd() + '/new-created-directory'
            const result = await host().adapter.executeHost({create_session:{cwd}})
            return {receipt:result, directory_created:fs.statSync(cwd).isDirectory()}
          }
          if (method === 'workspace-warning-after-persist') {
            const presets = ctx.get('agentPresets')
            const query = ctx.get('sessionQuery')
            const {workspace} = await ctx.workspaceController.create({path:process.cwd()})
            const preset = (await presets.resolve()).id
            const original = presets.mount
            let outcome, caught
            presets.mount = async () => { throw Object.assign(new Error('synthetic nested attachment failure'), {isDSHRemoteError:true, code:'session/workspace-attach-failed', details:{sessionId:'fixture-other-session',workspaceId:workspace.workspaceId}}) }
            try { outcome = await host().adapter.executeHost({create_session:{preferred_session_id:sessionId,workspace_id:workspace.workspaceId,agent_preset:preset}}) }
            catch (error) { caught = error }
            finally { presets.mount = original }
            let persisted = false
            try {
              const observation = await query.observeSession(sessionId)
              persisted = observation.header.id === sessionId
              await observation[Symbol.asyncDispose]()
            } catch {}
            return {reportedSession:outcome?.identity?.session_id ?? null, outcome, code:caught?.code, definitive:caught?.constructor.name === 'DefinitiveExecutionError', persisted}
          }
          if (method === 'workspace-error-after-persist') {
            const presets = ctx.get('agentPresets')
            const query = ctx.get('sessionQuery')
            const {workspace} = await ctx.workspaceController.create({path:process.cwd()})
            const preset = (await presets.resolve()).id
            const original = presets.mount
            let caught
            presets.mount = async () => { throw Object.assign(new Error('synthetic nested workspace failure'), {isDSHRemoteError:true, code:'workspace/not-found', details:{workspaceId:workspace.workspaceId}}) }
            let outcome
            try { outcome = await host().adapter.executeHost({create_session:{preferred_session_id:sessionId,workspace_id:workspace.workspaceId,agent_preset:preset}}) }
            catch (error) { caught = error }
            finally { presets.mount = original }
            let persisted = false
            try {
              const observation = await query.observeSession(sessionId)
              persisted = observation.header.id === sessionId
              await observation[Symbol.asyncDispose]()
            } catch {}
            return {code:caught?.code ?? outcome?.failure_code, outcome, identity:outcome?.identity?.session_id ?? null, definitive:caught?.constructor.name === 'DefinitiveExecutionError', persisted}
          }
          if (method === 'create-adapter') return host().adapter.executeHost({create_session:{}})
          if (method === 'create-native-default') return ctx.sessionController.create({})
          if (method === 'creation-hold-history-failure') {
            if (creationHistoryFault) throw new Error('history fault already armed')
            const adapter = host().adapter
            const original = adapter.readHistory
            const fault = {adapter, original, attempts: 0}
            creationHistoryFault = fault
            adapter.readHistory = async function(target, ...args) {
              if (target !== sessionId) return original.call(this, target, ...args)
              fault.attempts++
              throw new Error('synthetic history read failure')
            }
            return {armed:true}
          }
          if (method === 'creation-release-history-failure') {
            const fault = creationHistoryFault
            if (!fault) throw new Error('history fault not armed')
            fault.adapter.readHistory = fault.original
            creationHistoryFault = null
            return {restored:true, attempts:fault.attempts}
          }
          if (method === 'create-preferred') return host().adapter.executeHost({create_session:{preferred_session_id:sessionId}})
          if (method === 'create') return ctx.sessionController.create({ cwd: process.cwd() })
          if (method === 'model-catalog') return host().adapter.listModels()
          if (method === 'test-model-provider-failure-arm') {
            const llm = ctx.llm
            const originalProviders = llm.listProviders
            const originalModels = llm.listModels
            llm.listProviders = function () { return [...originalProviders.call(this), {id:'fixture-bad-provider',name:'Fixture failure'}] }
            llm.listModels = function (provider) {
              if (provider === 'fixture-bad-provider') throw new Error('fixture-provider-private-diagnostic')
              return originalModels.call(this, provider)
            }
            return {armed:true}
          }
          if (method === 'model-catalog-provider-failure') {
            const llm = ctx.llm
            const originalProviders = llm.listProviders
            const originalModels = llm.listModels
            llm.listProviders = function () { return [...originalProviders.call(this), { id: 'fixture-bad-provider', name: 'Fixture failure' }] }
            llm.listModels = function (provider) {
              if (provider === 'fixture-bad-provider') throw new Error('fixture-provider-private-diagnostic')
              return originalModels.call(this, provider)
            }
            try { return await host().adapter.listModels() }
            finally { llm.listProviders = originalProviders; llm.listModels = originalModels }
          }
          if (method === 'model-read') return host().adapter.readSessionModel(sessionId)
          if (method === 'model-select') return host().adapter.selectSessionModel(sessionId, selection)
          if (method === 'model-select-outcome') {
            try { return { accepted: true, receipt: await host().adapter.selectSessionModel(sessionId, selection) } }
            catch (error) { return { accepted: false, code: error?.code ?? 'internal', definitive: error?.name === 'DefinitiveExecutionError' } }
          }
          if (method === 'model-select-local') return ctx.sessionController.selectModel({ sessionId, provider: selection.provider, model: selection.model })
          if (method === 'model-select-concurrent') {
            const [remote, local] = await Promise.all([
              host().adapter.selectSessionModel(sessionId, selection),
              ctx.sessionController.selectModel({ sessionId, provider: otherSelection.provider, model: otherSelection.model }),
            ])
            return { remote, local }
          }
          if (method === 'model-select-save-observe') {
            const defaults = ctx.agentDefaultModel
            const original = defaults.saveSelection
            let attempts = 0
            let saved
            defaults.saveSelection = function (...args) {
              attempts++
              saved = original.apply(this, args)
              return saved
            }
            try {
              const receipt = await host().adapter.selectSessionModel(sessionId, selection)
              if (!saved) throw new Error('default save not attempted')
              await saved
              return { receipt, attempts, persisted: defaults.currentSelection() }
            } finally { defaults.saveSelection = original }
          }
          if (method === 'model-select-save-fails') {
            const defaults = ctx.agentDefaultModel
            const original = defaults.saveSelection
            let attempts = 0
            defaults.saveSelection = async () => { attempts++; throw new Error('isolated model save failure') }
            try {
              const receipt = await host().adapter.selectSessionModel(sessionId, selection)
              return { receipt, attempts }
            } finally { defaults.saveSelection = original }
          }
          if (method === 'test-model-default-failure-arm') {
            const defaults = ctx.agentDefaultModel
            const file = socketPath + '.model-save-attempts'
            defaults.saveSelection = async () => {
              const attempts = fs.existsSync(file) ? Number(fs.readFileSync(file, 'utf8')) : 0
              fs.writeFileSync(file, String(attempts + 1))
              throw new Error('isolated model save failure')
            }
            return {armed:true}
          }
          if (method === 'test-model-default-failure-count') {
            const file = socketPath + '.model-save-attempts'
            return {count:fs.existsSync(file) ? Number(fs.readFileSync(file, 'utf8')) : 0}
          }
          if (method === 'model-question-start') {
            const agent = ctx.agents.get(sessionId)
            if (!agent || questionState !== 'none') throw new Error('question fixture unavailable')
            ctx.userQuestions.ask({ agent, questions: [{ id: 'model-fixture-question', question: 'Fixture question' }] })
              .then(() => { questionState = 'answered' }, () => { questionState = 'failed' })
            await Promise.resolve()
            if (!questionResolve) throw new Error('question fixture did not start')
            return { pending: questionState === 'pending' }
          }
          if (method === 'model-question-state') return { state: questionState }
          if (method === 'model-question-answer') {
            if (!questionResolve) throw new Error('question fixture unavailable')
            questionResolve({ answers: [{ id: 'model-fixture-question', selected: [], custom: 'fixture answer' }] })
            questionResolve = null
            await Promise.resolve()
            return { state: questionState }
          }
          if (method === 'model-queue') {
            return ctx.sessionController.prompt({ requestId: 'model-fixture-queue', sessionId, mode: 'queue', content: [{ type: 'text', text: 'fixture queued message' }] }, signal)
          }
          if (method === 'question-enable-continued-subclass-fixture') {
            const runtime = host(), previous = runtime.adapter
            runtime.injectedAdapter = new ContinuedFixtureAdapter({ ctx: previous.ctx, config: previous.config, now: previous.now })
            await runtime.reload()
            await runtime.access.waitForConnected(15000)
            for (let attempt = 0; attempt < 150 && runtime.security.revocationsPending; attempt++) {
              await new Promise(resolve => setTimeout(resolve, 100))
            }
            if (runtime.security.revocationsPending) throw new Error('fixture host unavailable')
            return { enabled: true }
          }
          if (method === 'question-managed-root-create') {
            const id = `fixture-managed-${Date.now()}`, defaults = ctx.get('agentDefaultModel'), presets = ctx.get('agentPresets')
            const preset = presets ? (await presets.resolve()).id : undefined
            const options = { get provider() { return defaults.currentSelection().provider },
              get model() { return defaults.currentSelection().model },
              get reasoningEffort() { return defaults.currentSelection().reasoningEffort } }
            const setup = async agentCtx => { installModelSelection(agentCtx); if (presets) await presets.mount(agentCtx, preset) }
            const handle = await ctx.agents.create({ sessionId: id, agentOptions: options,
              meta: { cwd: process.cwd(), ...(preset ? { agentPreset: preset } : {}) }, setup })
            managedRoots.set(id, { handle, options, setup })
            return { sessionId: id }
          }
          if (method === 'question-managed-root-replace') {
            const entry = managedRoots.get(sessionId), previous = entry.handle.agent
            await entry.handle.dispose()
            entry.handle = await ctx.agents.resume({ resumeSessionId: sessionId, agentOptions: entry.options, setup: entry.setup })
            return { replaced: previous !== entry.handle.agent }
          }
          if (method === 'question-enable-remote-fixture') {
            const adapter = host().adapter
            adapter.config.enableOpenQuestionBroker = true
            await adapter.open()
            const original = adapter.describe.bind(adapter)
            adapter.describe = () => {
              const value = original()
              return { ...value, capabilities: [...value.capabilities, 'questions-read', 'questions-answer-open'] }
            }
            return { enabled: true }
          }
          if (method === 'question-mixed-start') {
            const agent = ctx.agents.get(sessionId), local = Promise.withResolvers()
            const fallback = ctx.on('user-questions/request', (request, next) =>
              request.agent === agent && request.questions?.[0]?.id === 'legacy' ? local.promise : next(),
              { global: true, prepend: true })
            const adapter = host().adapter
            adapter.config.enableOpenQuestionBroker = true
            await adapter.open()
            const pending = ctx.userQuestions.ask({ agent, questions: [{ id: 'legacy', question: 'Fixture legacy' }] })
            pending.catch(() => {})
            questionRuns.set(sessionId, { pending, local, fallback })
            await new Promise(resolve => setImmediate(resolve))
            return { started: true }
          }
          if (method === 'question-mixed-observe') {
            const page = await host().adapter.listQuestionPage(sessionId)
            return { ids: page.batches.map(batch => batch.questions?.[0]?.id ?? '') }
          }
          if (method === 'question-mixed-release') {
            const run = questionRuns.get(sessionId)
            run.local.resolve({ answers: [{ id: 'legacy', selected: [] }] })
            await run.pending
            run.fallback()
            questionRuns.delete(sessionId)
            return { released: true }
          }
          if (method === 'question-transition-capture') {
            const page = await host().adapter.listQuestionPage(sessionId)
            const batch = page.batches.find(item => item.phase === 'QUESTION_OPEN' && item.answerable)
            if (!batch) return { found: false }
            staleQuestions.set(sessionId, { session_id: sessionId, batch_id: batch.batch_id,
              source_epoch: batch.source_epoch, expected_version: batch.version, expected_digest: batch.digest,
              expected_phase: batch.phase, answers: [{ id: 'q1', selected: ['Yes'] }] })
            return { found: true }
          }
          if (method === 'question-transition-answer') {
            try { await host().adapter.answerOpenQuestion(staleQuestions.get(sessionId)); return { code: 'accepted' } }
            catch (error) { return { code: error?.code ?? 'internal', definitive: error?.name === 'DefinitiveExecutionError' } }
          }
          if (method === 'question-start-remote-fixture') {
            const agent = ctx.agents.get(sessionId)
            // Cases that must survive the Access request-deadline fallback ask
            // for a longer native question window; the default stays 30s.
            const questionMs = Number.isSafeInteger(config?.questionMs) && config.questionMs > 0 ? config.questionMs : 30000
            const pending = ctx.userQuestions.askTimed({ agent, questions: [{ id: 'q1', question: 'Fixture',
              options: [{ label: 'Yes' }, { label: 'No' }] }] }, 'remote-fixture', questionMs)
            questionRuns.set(sessionId, pending)
            pending.catch(() => {})
            await new Promise(resolve => setImmediate(resolve))
            return { started: true }
          }
          if (method === 'question-race-encrypted-start') {
            const agent = ctx.agents.get(sessionId)
            const pending = ctx.userQuestions.askTimed({ agent, questions: [{ id: 'q1', question: 'Fixture',
              options: [{ label: 'Yes' }, { label: 'No' }] }] }, 'encrypted-race', 30000)
            pending.catch(() => {})
            questionRuns.set(sessionId, pending)
            await new Promise(resolve => setImmediate(resolve))
            return { started: true }
          }
          if (method === 'question-race-encrypted-result') {
            const result = await questionRuns.get(sessionId)
            questionRuns.delete(sessionId)
            return { selected: result?.answers?.[0]?.selected?.[0] ?? '', pending: result?.pending === true }
          }
          if (method === 'question-multiple-native-start') {
            const agent = ctx.agents.get(sessionId)
            const pending = ctx.userQuestions.askTimed({ agent, questions: [
              { id: 'q1', question: 'First fixture', multiSelect: true, options: [{ label: 'A' }, { label: 'B' }, { label: 'C' }] },
              { id: 'q2', question: 'Second fixture', options: [{ label: 'Yes' }, { label: 'No' }] },
            ] }, 'multiple-fixture', 30000)
            questionRuns.set(sessionId, pending)
            pending.catch(() => {})
            await new Promise(resolve => setImmediate(resolve))
            return { started: true }
          }
          if (method === 'question-multiple-native-result') {
            const result = await questionRuns.get(sessionId)
            questionRuns.delete(sessionId)
            return { answers: result?.answers ?? [], pending: result?.pending === true }
          }
          if (method === 'question-reload-adapter-fixture') {
            const adapter = host().adapter
            await adapter.close()
            await adapter.open()
            return { reloaded: true }
          }
          if (method === 'question-hold-native-claim') {
            const service = ctx.userQuestions
            const original = service.attachWait
            const bound = original.bind(service)
            service.attachWait = (agent, callId, abortSignal) => (async function* () {
              const iterator = bound(agent, callId, abortSignal)
              let held = false
              try {
                const opening = await iterator.next()
                if (opening.done) return
                held = true; heldQuestionClaims++
                await new Promise((_, reject) => {
                  if (abortSignal.aborted) return reject(abortSignal.reason)
                  abortSignal.addEventListener('abort', () => reject(abortSignal.reason), { once: true })
                })
                yield opening.value
              } finally {
                if (held) heldQuestionClaims--
                await iterator.return()
              }
            })()
            restoreAttachWait = () => { service.attachWait = original; restoreAttachWait = null }
            return { installed: true }
          }
          if (method === 'presence-event-count') return { count: host().presenceEvents ?? 0 }
          if (method === 'simulate-device-offline') {
            const target = config?.deviceId ?? deviceId
            if (typeof target !== 'string' || !target) throw new Error('deviceId required')
            host().notifyPresence({ event: 'device_offline', device_id: target, reason: 'fixture' })
            return { simulated: true }
          }
          if (method === 'question-native-claim-status') return { held: heldQuestionClaims }
          if (method === 'question-restore-native-claim') { restoreAttachWait?.(); return { restored: true } }
          if (method === 'question-result-remote-fixture') {
            const result = await questionRuns.get(sessionId)
            questionRuns.delete(sessionId)
            return { selected: result?.answers?.[0]?.selected?.[0] ?? '', pending: result?.pending === true }
          }
          if (method === 'question-result-remote-shape') {
            const result = await questionRuns.get(sessionId)
            questionRuns.delete(sessionId)
            return { selected: result?.answers?.[0]?.selected ?? [], custom: result?.answers?.[0]?.custom ?? null }
          }
          if (method === 'question-root-probe') {
            const agent = ctx.agents.get(sessionId)
            if (!agent || !ctx.agents.roots().includes(agent)) throw new Error('root unavailable')
            let observed = 0
            const dispose = ctx.on('user-questions/request', (request, next) => {
              if (request.agent !== agent) return next()
              observed++
              return Promise.resolve({ answers: [{ id: 'fixture-q', selected: ['Yes'] }] })
            }, { global: true, prepend: true })
            try {
              const reply = await ctx.userQuestions.ask({ agent, questions: [{ id: 'fixture-q', question: 'Fixture', options: [{ label: 'Yes' }] }] })
              const absentContinued = ctx.userQuestions.answer(agent, 'missing-fixture-call', { answers: [] })
              let stale = ''
              try { ctx.userQuestions.answer({ id: agent.id }, 'missing-fixture-call', { answers: [] }) }
              catch (error) { stale = error?.code ?? '' }
              const source = host().adapter.describe()
              return { observed, answered: reply?.answers?.[0]?.selected?.[0] === 'Yes', absentContinued, stale,
                openEnabled: source.capabilities.includes('questions-answer-open'),
                claimed: host().adapter.listClaimedQuestions(sessionId).length }
            } finally { dispose() }
          }
          if (method === 'question-owned-child-probe') {
            const parent = ctx.agents.get(sessionId)
            const childHandle = await ctx.agents.create({ sessionId: `fixture-child-${Date.now()}`,
              parentAgent: parent, meta: { cwd: process.cwd() } })
            try {
              const child = childHandle.agent
              let code = ''
              try { await ctx.userQuestions.ask({ agent: child, questions: [{ id: 'q1', question: 'Fixture' }] }) }
              catch (error) { code = error?.code ?? 'internal' }
              const broker = new OpenQuestionBroker({ agents: ctx.agents, userQuestions: ctx.userQuestions, handles: new QuestionHandles() })
              const result = await broker.capture({ agent: child, questions: [{ id: 'q1', question: 'Fixture' }],
                wait: { callId: 'child-fixture' } }, async () => 'delegated')
              const active = broker.list(child.id).length
              let readCode = ''
              try { await host().adapter.listProjectedQuestions(child.id) } catch (error) { readCode = error?.code ?? 'internal' }
              broker.close()
              return { owned: ctx.agents.isOwnedBy(child.id, parent), isRoot: ctx.agents.roots().includes(child), code, result, active, readCode }
            } finally { await childHandle.dispose() }
          }
          if (method === 'question-resumed-root-probe') {
            const parent = ctx.agents.get(sessionId), childId = `fixture-lineage-${Date.now()}`
            const first = await ctx.agents.create({ sessionId: childId, parentAgent: parent,
              meta: { cwd: process.cwd(), parentSession: sessionId, origin: 'subagent' } })
            await first.dispose()
            const resumed = await ctx.agents.resume({ resumeSessionId: childId })
            const agent = resumed.agent
            const local = ctx.on('user-questions/request', (request, next) => request.agent === agent
              ? Promise.resolve({ answers: [{ id: 'q1', selected: [] }] }) : next(), { global: true, prepend: true })
            try {
              const answer = await ctx.userQuestions.ask({ agent, questions: [{ id: 'q1', question: 'Fixture' }] })
              const broker = new OpenQuestionBroker({ agents: ctx.agents, userQuestions: ctx.userQuestions, handles: new QuestionHandles() })
              const pending = Promise.withResolvers()
              const native = broker.capture({ agent, questions: [{ id: 'q1', question: 'Fixture' }] }, () => pending.promise)
              const answerable = broker.list(childId).length
              pending.resolve({ answers: [] }); await native; broker.close()
              return { isRoot: ctx.agents.roots().includes(agent), hasParent: agent.session.header.parentSession === sessionId,
                answered: answer.answers[0].id === 'q1', answerable }
            } finally { local(); await resumed.dispose() }
          }
          if (method === 'question-replaced-root-guard-probe') {
            const id = `fixture-replaced-${Date.now()}`
            const first = await ctx.agents.create({ sessionId: id, meta: { cwd: process.cwd() } })
            const a = first.agent
            const fixture = { agents: ctx.agents, sessions: ctx.sessions, userQuestions: ctx.userQuestions,
              sessionController: { projections: async () => ({ kind: 'sequenced', values: { userQuestions: {
                active: [{ callId: 'fixture-call', state: 'continued', questions: [{ id: 'q1', question: 'Fixture' }] }],
              } } }) } }
            const adapter = new RealDshAdapter({ ctx: fixture })
            const before = (await adapter.listProjectedQuestions(id))[0]
            const reread = (await adapter.listProjectedQuestions(id))[0]
            const broker = new OpenQuestionBroker({ agents: ctx.agents, userQuestions: ctx.userQuestions, handles: new QuestionHandles() })
            const local = Promise.withResolvers()
            const pending = broker.capture({ agent: a, questions: [{ id: 'q1', question: 'Fixture' }] }, () => local.promise)
            const open = broker.list(id)[0]
            await first.dispose()
            const resumed = await ctx.agents.resume({ resumeSessionId: id })
            try {
              const after = (await adapter.listProjectedQuestions(id))[0]
              const request = batch => ({ session_id: id, batch_id: batch.batch_id, source_epoch: batch.source_epoch,
                expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
                answers: [{ id: 'q1', selected: [] }] })
              let continuedCode = '', openCode = ''
              try { await adapter.answerContinuedQuestion(request(before)) } catch (error) { continuedCode = error.code }
              try { await broker.answer(request(open)) } catch (error) { openCode = error.code }
              return { differentAgent: a !== resumed.agent, stableRead: before.version === reread.version && Buffer.from(before.digest).equals(Buffer.from(reread.digest)),
                changedGuard: after.version > before.version && !Buffer.from(after.digest).equals(Buffer.from(before.digest)),
                continuedCode, openCode, newAnswerable: after.answerable }
            } finally { local.resolve({ answers: [] }); await pending; broker.close(); await resumed.dispose() }
          }
          if (method === 'question-claim-probe') {
            const agent = ctx.agents.get(sessionId)
            if (!agent || !ctx.agents.roots().includes(agent)) throw new Error('root unavailable')
            const handles = new QuestionHandles()
            const events = []
            handles.subscribe(event => events.push(event.type))
            const ready = Promise.withResolvers()
            const release = Promise.withResolvers()
            let localCalls = 0
            const local = ctx.on('user-questions/request', (request, next) => {
              if (request.agent !== agent || request.wait?.callId !== 'fixture-wait') return next()
              localCalls++
              return Promise.resolve({ answers: [{ id: 'q1', selected: ['Yes'] }] })
            }, { global: true, prepend: true })
            const remote = ctx.on('user-questions/request', async (request, next) => {
              if (request.agent !== agent || request.wait?.callId !== 'fixture-wait') return next()
              const claim = new AbortController()
              const iterator = ctx.userQuestions.attachWait(agent, 'fixture-wait', claim.signal)
              const frame = await iterator.next()
              if (frame.done) throw new Error('native wait unavailable')
              const handle = handles.register(sessionId, request.wait.callId, () => claim.abort())
              ready.resolve(handle)
              await release.promise
              handle.release()
              await iterator.return()
              return next()
            }, { global: true, prepend: true })
            try {
              const pending = ctx.userQuestions.askTimed({ agent, questions: [{ id: 'q1', question: 'Fixture', options: [{ label: 'Yes' }] }] }, 'fixture-wait', 5000)
              const handle = await ready.promise
              const own = handles.listActive(sessionId).length
              const other = handles.listActive('unrelated-session').length
              release.resolve()
              const answer = await pending
              return { own, other, after: handles.listActive(sessionId).length, localCalls,
                events, answered: answer?.answers?.[0]?.selected?.[0] === 'Yes', releasedAgain: handle.release() }
            } finally { handles.close(); remote(); local() }
          }
          if (method === 'question-open-race-probe') {
            const agent = ctx.agents.get(sessionId)
            const handles = new QuestionHandles()
            const opened = Promise.withResolvers(), continueOpen = Promise.withResolvers()
            let actualClaims = 0
            const nativeQuestions = config?.winner === 'disconnect' ? {
              async *attachWait(owner, callId, signal) {
                const iterator = ctx.userQuestions.attachWait(owner, callId, signal)
                try {
                  const frame = await iterator.next()
                  if (frame.done) return
                  actualClaims++
                  opened.resolve()
                  await continueOpen.promise
                  yield frame.value
                } finally { actualClaims--; await iterator.return() }
              },
            } : ctx.userQuestions
            const broker = new OpenQuestionBroker({ agents: ctx.agents, userQuestions: nativeQuestions, handles })
            const local = Promise.withResolvers()
            let localCalls = 0
            let providerHops = 0
            const fallback = ctx.on('user-questions/request', (request, next) => {
              if (request.agent !== agent || request.wait?.callId !== 'race-fixture') return next()
              localCalls++
              if (config?.winner === 'expiry') return next()
              return local.promise
            }, { global: true, prepend: true })
            const passthrough = ctx.on('user-questions/request', (request, next) => {
              if (request.agent !== agent || request.wait?.callId !== 'race-fixture') return next()
              providerHops++
              return next()
            }, { global: true, prepend: true })
            const intercept = ctx.on('user-questions/request', (request, next) => {
              if (request.agent !== agent || request.wait?.callId !== 'race-fixture') return next()
              return broker.capture(request, next)
            }, { global: true, prepend: true })
            const lifetime = new AbortController()
            try {
              const pending = ctx.userQuestions.askTimed({ agent, signal: lifetime.signal,
                questions: [{ id: 'q1', question: 'Fixture', options: [{ label: 'Yes' }, { label: 'No' }] }] }, 'race-fixture', config?.winner === 'expiry' ? 200 : 5000)
              await new Promise(resolve => setImmediate(resolve))
              const [batch] = broker.list(sessionId)
              if (!batch) throw new Error('no open handle')
              const answer = { session_id: sessionId, batch_id: batch.batch_id, source_epoch: batch.source_epoch,
                expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
                answers: [{ id: 'q1', selected: ['Yes'] }] }
              let receipt = null, lateCode = ''
              if (config?.winner === 'expiry') {
                const result = await pending
                try { await broker.answer(answer) } catch (error) { lateCode = error?.code ?? 'internal' }
                return { pending: result.pending === true, lateCode, localCalls, providerHops, active: broker.list(sessionId).length,
                  claimed: handles.listActive(sessionId).length }
              }
              if (config?.winner === 'cancel') {
                lifetime.abort()
                let code = ''
                try { await pending } catch (error) { code = error?.code ?? 'internal' }
                return { code, localCalls, providerHops, active: broker.list(sessionId).length, claimed: handles.listActive(sessionId).length }
              }
              if (config?.winner === 'disconnect') {
                const channel = new AbortController()
                const attempt = broker.answer(answer, channel.signal)
                await opened.promise
                channel.abort()
                continueOpen.resolve()
                try { await attempt } catch (error) { lateCode = error?.code ?? 'internal' }
                local.resolve({ answers: [{ id: 'q1', selected: ['No'] }] })
                const result = await pending
                return { selected: result.answers[0].selected[0], localCalls, providerHops, lateCode, actualClaims,
                  active: broker.list(sessionId).length, claimed: handles.listActive(sessionId).length }
              }
              if (config?.winner === 'local') {
                local.resolve({ answers: [{ id: 'q1', selected: ['No'] }] })
                const result = await pending
                try { await broker.answer(answer) } catch (error) { lateCode = error?.code ?? 'internal' }
                return { selected: result.answers[0].selected[0], localCalls, providerHops, lateCode, active: broker.list(sessionId).length,
                  claimed: handles.listActive(sessionId).length }
              }
              if (config?.winner === 'simultaneous') {
                const attempts = [broker.answer(answer), broker.answer(answer)]
                local.resolve({ answers: [{ id: 'q1', selected: ['No'] }] })
                const outcomes = await Promise.allSettled(attempts)
                const result = await pending
                return { selected: result.answers[0].selected[0], localCalls, providerHops,
                  successes: outcomes.filter(outcome => outcome.status === 'fulfilled').length,
                  active: broker.list(sessionId).length, claimed: handles.listActive(sessionId).length }
              }
              receipt = await broker.answer(answer)
              const result = await pending
              local.resolve({ answers: [{ id: 'q1', selected: ['No'] }] })
              return { selected: result.answers[0].selected[0], localCalls, providerHops, phase: receipt.phase,
                active: broker.list(sessionId).length, claimed: handles.listActive(sessionId).length }
            } finally { lifetime.abort(); broker.close(); intercept(); passthrough(); fallback() }
          }
          if (method === 'question-future-request-probe') {
            const agent = ctx.agents.get(sessionId), handles = new QuestionHandles()
            const broker = new OpenQuestionBroker({ agents: ctx.agents, userQuestions: ctx.userQuestions, handles })
            const firstLocal = Promise.withResolvers(), secondLocal = Promise.withResolvers()
            const fallback = ctx.on('user-questions/request', (request, next) => {
              if (request.agent !== agent || request.wait?.callId !== 'reused-call') return next()
              return request.questions[0].id === 'first' ? firstLocal.promise : secondLocal.promise
            }, { global: true, prepend: true })
            const intercept = ctx.on('user-questions/request', (request, next) =>
              request.agent === agent && request.wait?.callId === 'reused-call' ? broker.capture(request, next) : next(),
              { global: true, prepend: true })
            try {
              const first = ctx.userQuestions.askTimed({ agent, questions: [{ id: 'first', question: 'Fixture',
                options: [{ label: 'Yes' }] }] }, 'reused-call', 5000)
              await new Promise(resolve => setImmediate(resolve))
              const [old] = broker.list(sessionId)
              const stale = { session_id: sessionId, batch_id: old.batch_id, source_epoch: old.source_epoch,
                expected_version: old.version, expected_digest: old.digest, expected_phase: old.phase,
                answers: [{ id: 'first', selected: ['Yes'] }] }
              await broker.answer(stale)
              await first
              const second = ctx.userQuestions.askTimed({ agent, questions: [{ id: 'second', question: 'Fixture',
                options: [{ label: 'No' }] }] }, 'reused-call', 5000)
              await new Promise(resolve => setImmediate(resolve))
              const [fresh] = broker.list(sessionId)
              let staleCode = ''
              try { await broker.answer(stale) } catch (error) { staleCode = error?.code ?? 'internal' }
              secondLocal.resolve({ answers: [{ id: 'second', selected: ['No'] }] })
              const result = await second
              firstLocal.resolve({ answers: [] })
              return { distinct: fresh.batch_id !== old.batch_id, staleCode, selected: result.answers[0].selected[0],
                active: broker.list(sessionId).length, claimed: handles.listActive(sessionId).length }
            } finally { broker.close(); intercept(); fallback() }
          }
          if (method === 'question-deadline-probe') {
            const agent = ctx.agents.get(sessionId), handles = new QuestionHandles(), local = Promise.withResolvers()
            let now = 100, nativeClaims = 0
            const service = { async *attachWait(owner, callId, signal) {
              const iterator = ctx.userQuestions.attachWait(owner, callId, signal)
              let held = false
              try {
                const frame = await iterator.next()
                if (frame.done) return
                held = true; nativeClaims++; now = 150
                yield frame.value
              } finally { if (held) nativeClaims--; await iterator.return() }
            } }
            const broker = new OpenQuestionBroker({ agents: ctx.agents, userQuestions: service, handles, now: () => now })
            const fallback = ctx.on('user-questions/request', (request, next) =>
              request.agent === agent && request.wait?.callId === 'deadline-fixture' ? local.promise : next(),
              { global: true, prepend: true })
            const intercept = ctx.on('user-questions/request', (request, next) =>
              request.agent === agent && request.wait?.callId === 'deadline-fixture' ? broker.capture(request, next) : next(),
              { global: true, prepend: true })
            try {
              const pending = ctx.userQuestions.askTimed({ agent, questions: [{ id: 'q1', question: 'Fixture',
                options: [{ label: 'Yes' }, { label: 'No' }] }] }, 'deadline-fixture', 5000)
              await new Promise(resolve => setImmediate(resolve))
              const [batch] = broker.list(sessionId)
              let code = '', definitive = false
              try { await broker.answer({ session_id: sessionId, batch_id: batch.batch_id, source_epoch: batch.source_epoch,
                expected_version: batch.version, expected_digest: batch.digest, expected_phase: batch.phase,
                answers: [{ id: 'q1', selected: ['Yes'] }] }, undefined, { expiresAt: 150 }) }
              catch (error) { code = error?.code ?? 'internal'; definitive = error?.name === 'DefinitiveExecutionError' }
              local.resolve({ answers: [{ id: 'q1', selected: ['No'] }] })
              const result = await pending
              return { code, definitive, selected: result.answers[0].selected[0], nativeClaims,
                claimed: handles.listActive(sessionId).length }
            } finally { broker.close(); intercept(); fallback() }
          }
          if (method === 'question-context-probe') {
            const agent = ctx.agents.get(sessionId), handles = new QuestionHandles()
            const broker = new OpenQuestionBroker({ agents: ctx.agents, userQuestions: ctx.userQuestions, handles })
            const local = Promise.withResolvers()
            let localAborted = 0
            const kind = config?.kind === 'approval' ? 'approval' : 'plan-review'
            const intent = kind === 'approval' ? { kind, approve: 'Allow', reject: 'Deny', subject: { name: 'Fixture' },
              display: { question: { en: 'Fixture display' } } } : { kind, approve: 'Allow', callId: 'plan-call' }
            const question = { id: 'q1', question: 'Fixture', detail: 'Fixture detail', header: 'Fixture header',
              options: [{ label: 'Allow', description: 'Allow fixture' }, { label: 'Deny', description: 'Deny fixture' }], intent }
            const fallback = ctx.on('user-questions/request', (request, next) => {
              if (request.agent !== agent) return next()
              return new Promise((resolve, reject) => {
                request.signal?.addEventListener('abort', () => { localAborted++; reject(new Error('local fixture released')) }, { once: true })
                local.promise.then(resolve, reject)
              })
            }, { global: true, prepend: true })
            const intercept = ctx.on('user-questions/request', (request, next) => request.agent === agent ? broker.capture(request, next) : next(), { global: true, prepend: true })
            try {
              const native = ctx.userQuestions.ask({ agent, questions: [question] })
              await new Promise(resolve => setImmediate(resolve))
              const [batch] = broker.list(sessionId)
              if (!batch) throw new Error('no live question')
              const normalized = batch.questions[0]
              const receipt = await broker.answer({ session_id: sessionId, batch_id: batch.batch_id,
                source_epoch: batch.source_epoch, expected_version: batch.version, expected_digest: batch.digest,
                expected_phase: batch.phase, answers: [{ id: 'q1', selected: ['Deny'] }] })
              const result = await native
              local.resolve({ answers: [] })
              return { kind: normalized.intent.kind, detail: normalized.detail === 'Fixture detail',
                description: normalized.options[1].description === 'Deny fixture',
                display: kind !== 'approval' || normalized.intent.display.question.translations.en === 'Fixture display',
                selected: result.answers[0].selected[0], phase: receipt.phase, claimed: handles.listActive(sessionId).length, localAborted }
            } finally { broker.close(); intercept(); fallback() }
          }
          if (method === 'question-projected') {
            const batches = await host().adapter.listProjectedQuestions(sessionId)
            return { count: batches.length, phases: batches.map(batch => batch.phase), answerable: batches.map(batch => batch.answerable) }
          }
          if (method === 'question-answer-probe') {
            const batch = (await host().adapter.listProjectedQuestions(sessionId))[0]
            if (!batch) throw new Error('question unavailable')
            const result = await host().adapter.answerContinuedQuestion({ session_id: sessionId, batch_id: batch.batch_id,
              source_epoch: batch.source_epoch, expected_version: batch.version, expected_digest: batch.digest,
              expected_phase: batch.phase, answers: [{ id: 'q1', selected: ['Yes'] }] })
            return { phase: result.phase }
          }
          if (method === 'question-continued-status') {
            const agent = ctx.agents.get(sessionId)
            const active = ctx.userQuestions.continued(agent)
            const pending = [...agent.inbox.nextTurn, ...agent.inbox.nextStep]
              .filter(message => message.source?.kind === 'user-question-reply')
            return { continued: active.length, queued: pending.length }
          }
          if (method === 'question-continued-multi-proof') {
            const agent = ctx.agents.get(sessionId)
            const messages = [...agent.inbox.nextTurn, ...agent.inbox.nextStep]
              .filter(message => message.source?.kind === 'user-question-reply')
            if (messages.length !== 1) return { oneReply: false, answerMatches: false }
            try {
              const text = messages[0].content?.find(block => block.type === 'text')?.text
              const payload = JSON.parse(text)
              const expected = [{ id: 'q2', selected: ['No'], custom: 'Fixture custom' }, { id: 'q1', selected: ['B', 'A'] }]
              return { oneReply: true, answerMatches: JSON.stringify(payload.answers) === JSON.stringify(expected) }
            } catch { return { oneReply: true, answerMatches: false } }
          }
          if (method === 'question-continued-duplicate') {
            const agent = ctx.agents.get(sessionId), [question] = ctx.userQuestions.continued(agent)
            if (!question) throw new Error('continued question unavailable')
            try { return { accepted: ctx.userQuestions.answer(agent, question.callId, { answers: [{ id: 'q1', selected: ['Yes'] }] }) } }
            catch (error) { return { code: error?.code ?? 'internal' } }
          }
          if (method === 'question-native-answer-shape') {
            const agent = ctx.agents.get(sessionId), [question] = ctx.userQuestions.continued(agent)
            if (!question) throw new Error('continued question unavailable')
            const value = config?.shape === 'unknown' ? { id: 'q1', selected: ['Unknown'] }
              : config?.shape === 'duplicate' ? { id: 'q1', selected: ['Yes', 'Yes'] }
              : config?.shape === 'multiple' ? { id: 'q1', selected: ['Yes', 'No'] }
              : { id: 'q1', selected: [], custom: 'Fixture custom' }
            try { return { accepted: ctx.userQuestions.answer(agent, question.callId, { answers: [value] }) } }
            catch (error) { return { code: error?.code ?? 'internal' } }
          }
          if (method === 'question-native-open-shape') {
            const agent = ctx.agents.get(sessionId)
            const shape = config?.shape
            const value = shape === 'unknown' ? { id: 'q1', selected: ['Unknown'] }
              : shape === 'duplicate' ? { id: 'q1', selected: ['Yes', 'Yes'] }
              : shape === 'multiple' ? { id: 'q1', selected: ['Yes', 'No'] }
              : { id: 'q1', selected: [], custom: 'Fixture custom' }
            const question = { id: 'q1', question: 'Fixture', options: [{ label: 'Yes' }, { label: 'No' }],
              ...(shape === 'approval-custom' ? { detail: 'Fixture scope', intent: { kind: 'approval', approve: 'Yes', reject: 'No' } } : {}) }
            const dispose = ctx.on('user-questions/request', (request, next) => request.agent === agent
              ? Promise.resolve({ answers: [value] }) : next(), { global: true, prepend: true })
            try {
              const result = await ctx.userQuestions.ask({ agent, questions: [question] })
              return { accepted: result.answers.length === 1, selectedCount: result.answers[0].selected.length,
                customPresent: result.answers[0].custom !== undefined }
            } finally { dispose() }
          }
          if (method === 'start') return host().adapter.sendUserMessage(sessionId, 'native integration fixture')
          if (method === 'arm-precheck-running-race') {
            const adapter = host().adapter
            const original = adapter.getSession.bind(adapter)
            adapter.getSession = async id => {
              adapter.getSession = original
              const raceSignal = AbortSignal.timeout(15000)
              await ctx.sessionController.prompt({requestId: 'fixture-local-before-precheck', sessionId: id, mode: 'queue',
                content: [{type: 'text', text: 'synthetic local start before observation'}]}, raceSignal)
              for (let attempt = 0; attempt < 100; attempt++) {
                if ((await ctx.sessionController.state({sessionId: id}, raceSignal)).running) return original(id)
                await new Promise(resolve => setTimeout(resolve, 20))
              }
              throw new Error('local race did not start')
            }
            return {armed: true}
          }
          if (method === 'arm-start-if-idle-race') {
            const adapter = host().adapter
            const original = adapter.getSession.bind(adapter)
            adapter.getSession = async id => {
              adapter.getSession = original
              const sampled = await original(id)
              const raceSignal = AbortSignal.timeout(15000)
              await ctx.sessionController.prompt({requestId: 'fixture-local-race', sessionId: id, mode: 'queue',
                content: [{type: 'text', text: 'synthetic concurrent local start'}]}, raceSignal)
              for (let attempt = 0; attempt < 100; attempt++) {
                if ((await ctx.sessionController.state({sessionId: id}, raceSignal)).running) return sampled
                await new Promise(resolve => setTimeout(resolve, 20))
              }
              throw new Error('local race did not start')
            }
            return { armed: true }
          }
          if (method === 'arm-native-unconfirmed-after-admission') {
            // Test-only fault: let the native command enter and alter its receipt.
            const raw = ctx.sessionController[Symbol.for('cordis.original')]
            const commands = raw.commands
            const descriptor = Object.getOwnPropertyDescriptor(commands, 'prompt')
            const original = commands.prompt.bind(commands)
            commands.prompt = async (...args) => {
              if (descriptor) Object.defineProperty(commands, 'prompt', descriptor)
              else delete commands.prompt
              await original(...args)
              if (config?.kind === 'missing') return undefined
              if (config?.kind === 'negative') return {accepted: false}
              return {}
            }
            return {armed: true}
          }
          if (method === 'delivery-entrypoint-symlink') {
            try { return {symlink: fs.lstatSync(process.argv[1]).isSymbolicLink()} }
            catch { return {symlink: false} }
          }
          if (method === 'arm-delivery-manifest-mutation') {
            const file = installed.resolve('@deepseek-ai/dsh-api-session-controller/package.json')
            const original = fs.readFileSync(file)
            const backup = `${file}.whalerein-test-backup`
            deliveryManifestMutation = {file, original, backup}
            try {
              if (config?.kind === 'missing') fs.renameSync(file, backup)
              else {
                const manifest = JSON.parse(original.toString('utf8'))
                manifest.version = '0.2.2-rc.2'
                fs.writeFileSync(file, JSON.stringify(manifest))
              }
              return {armed: true}
            } catch (error) {
              fs.writeFileSync(file, original)
              fs.rmSync(backup, {force: true})
              deliveryManifestMutation = null
              throw error
            }
          }
          if (method === 'restore-delivery-manifest') {
            const mutation = deliveryManifestMutation
            if (!mutation) return {restored: false}
            fs.writeFileSync(mutation.file, mutation.original)
            fs.rmSync(mutation.backup, {force: true})
            deliveryManifestMutation = null
            return {restored: true}
          }
          if (method === 'arm-delivery-service-replacement') {
            const modulePath = installed.resolve('@deepseek-ai/dsh-api-session-controller')
            const root = path.dirname(path.dirname(modulePath))
            const temp = fs.mkdtempSync('/tmp/dsh-controller-copy-')
            try {
              fs.cpSync(root, path.join(temp, 'controller'), {recursive: true})
              fs.symlinkSync('/opt/native/node_modules', path.join(temp, 'node_modules'), 'dir')
              const {SessionController} = await import(pathToFileURL(path.join(temp, 'controller', 'lib', 'index.js')).href)
              const adapter = host().adapter
              const previous = adapter.ctx
              const source = previous.sessionController
              const raw = Object.create(SessionController.prototype)
              raw.ctx = {fiber: {runtime: {callback: SessionController}}}
              raw.list = source.list.bind(source)
              const service = new Proxy(raw, {get(target, key, receiver) {
                return key === Symbol.for('cordis.original') ? target : Reflect.get(target, key, receiver)
              }})
              adapter.ctx = new Proxy(previous, {get(target, key, receiver) {
                return key === 'sessionController' ? service : Reflect.get(target, key, receiver)
              }})
              deliveryServiceReplacement = {adapter, previous, temp}
              return {armed: true}
            } catch (error) {
              fs.rmSync(temp, {recursive: true, force: true})
              throw error
            }
          }
          if (method === 'restore-delivery-service') {
            const replacement = deliveryServiceReplacement
            if (!replacement) return {restored: false}
            replacement.adapter.ctx = replacement.previous
            fs.rmSync(replacement.temp, {recursive: true, force: true})
            deliveryServiceReplacement = null
            return {restored: true}
          }
          if (method === 'arm-delivery-prompt-replacement') {
            const raw = ctx.sessionController[Symbol.for('cordis.original')]
            const descriptor = Object.getOwnPropertyDescriptor(raw, 'prompt')
            let calls = 0
            raw.prompt = async () => { calls++; return {accepted: true} }
            deliveryPromptReplacement = {raw, descriptor, count: () => calls}
            return {armed: true}
          }
          if (method === 'delivery-prompt-replacement-status') {
            return {calls: deliveryPromptReplacement?.count() ?? 0}
          }
          if (method === 'restore-delivery-prompt') {
            const replacement = deliveryPromptReplacement
            if (!replacement) return {restored: false}
            if (replacement.descriptor) Object.defineProperty(replacement.raw, 'prompt', replacement.descriptor)
            else delete replacement.raw.prompt
            deliveryPromptReplacement = null
            return {restored: true}
          }
          if (method === 'arm-native-unconfirmed-delivery') {
            const controller = ctx.sessionController
            const original = controller.prompt.bind(controller)
            controller.prompt = async (...args) => {
              controller.prompt = original
              await original(...args)
              if (config?.kind === 'missing') return undefined
              if (config?.kind === 'negative') return {accepted: false}
              return {}
            }
            return {armed: true}
          }
          if (method === 'arm-delivery-receipt-loss') {
            const adapter = host().adapter
            const original = adapter.sendUserMessage.bind(adapter)
            adapter.sendUserMessage = async (...args) => {
              adapter.sendUserMessage = original
              await original(...args)
              throw new Error('synthetic receipt loss')
            }
            return {armed: true}
          }
          if (method === 'arm-delivery-barrier') {
            const adapter = host().adapter
            const original = adapter.sendUserMessage.bind(adapter)
            deliveryBarrier = {entered: false}
            deliveryBarrier.promise = new Promise(resolve => { deliveryBarrier.release = resolve })
            adapter.sendUserMessage = async (...args) => {
              adapter.sendUserMessage = original
              deliveryBarrier.entered = true
              await deliveryBarrier.promise
              return original(...args)
            }
            return {armed: true}
          }
          if (method === 'delivery-barrier-state') return {entered: deliveryBarrier?.entered === true}
          if (method === 'release-delivery-barrier') { deliveryBarrier?.release(); return {released: true} }
          if (method === 'arm-abort-steer-observation') {
            const agent = ctx.agents.get(sessionId)
            if (!agent || !deliveryBarrier) throw new Error('agent or barrier unavailable')
            const cancel = agent.cancel.bind(agent)
            const steer = agent.steer.bind(agent)
            abortSteerObservation = null
            agent.cancel = (...args) => {
              agent.cancel = cancel
              const result = cancel(...args)
              deliveryBarrier.release()
              return result
            }
            agent.steer = message => {
              agent.steer = steer
              const phase = agent.phase
              steer(message)
              abortSteerObservation = {
                aborted: phase.kind !== 'idle' && phase.abort.signal.aborted,
                nextTurn: agent.inbox.nextTurn.some(item => item.id === message.id),
              }
            }
            return {armed: true}
          }
          if (method === 'abort-steer-observation') return abortSteerObservation ?? {pending: true}
          if (method === 'enable-delivery-for-test') {
            host().adapter.deliveryModes = ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER']
            return { enabled: true }
          }
          if (method === 'delivery-event-identities') {
            const page = await ctx.sessionController.events({address: {kind: 'session', sessionId}, maxMessages: 500}, signal)
            return {ids: (page.records ?? []).map(item => item.event)
              .filter(event => event?.type === 'user/message' && event.data?.source?.kind === 'user')
              .map(event => event.data.source.rpcId).filter(Boolean)}
          }
          if (method === 'inbox-metadata') {
            const state = await ctx.sessionController.state({ sessionId }, signal)
            return { ids: (state.queue ?? []).map(item => item.rpcId).filter(Boolean) }
          }
          if (method === 'queue-add') {
            await host().adapter.sendUserMessage(sessionId, 'pending fixture')
            return {accepted:true}
          }
          if (method === 'arm-queue-steer-failure') {
            const agent=ctx.agents.get(sessionId)
            if (!agent) throw new Error('agent unavailable')
            const original=agent.steer
            agent.steer=function(message){agent.steer=original;throw new RemoteError('session/steer-unavailable','synthetic post-removal failure',{itemId:message.id})}
            return {armed:true}
          }
          if (method === 'count-queue-mutations') {
            const adapter=host().adapter
            const original=adapter.updatePendingQueue.bind(adapter)
            const counter={calls:0}
            channelQueueMutationCounter=counter
            adapter.updatePendingQueue=async (...args)=>{counter.calls++;return original(...args)}
            return {armed:true}
          }
          if (method === 'queue-mutation-count') return {calls:channelQueueMutationCounter?.calls ?? 0}
          if (method === 'count-queue-reads') {
            const adapter=host().adapter
            const original=adapter.readPendingQueue.bind(adapter)
            const counter={calls:0}
            channelQueueReadCounter=counter
            adapter.readPendingQueue=async (...args)=>{counter.calls++;return original(...args)}
            return {armed:true}
          }
          if (method === 'queue-read-count') return {calls:channelQueueReadCounter?.calls ?? 0}
          if (method === 'arm-channel-queue-barrier') {
            const adapter=host().adapter
            const beforeNative=config?.phase === 'pre-native'
            const key=beforeNative ? 'readPendingQueue' : 'updatePendingQueue'
            const original=adapter[key].bind(adapter)
            const barrier={entered:false}
            barrier.promise=new Promise(resolve=>{barrier.release=resolve})
            channelQueueBarrier=barrier
            adapter[key]=async (...args)=>{
              adapter[key]=original
              if (!beforeNative) { const result=await original(...args);barrier.entered=true;await barrier.promise;return result }
              barrier.entered=true;await barrier.promise;return original(...args)
            }
            return {armed:true}
          }
          if (method === 'channel-queue-barrier') return {entered:channelQueueBarrier?.entered===true}
          if (method === 'release-channel-queue-barrier') { channelQueueBarrier?.release();return {released:true} }
          if (method === 'gx-fail-session-list') { const a=host().adapter; gxOriginalSessionList=a.listSessions; a.listSessions=async()=>{throw new Error('synthetic source unavailable')}; return {armed:true} }
          if (method === 'gx-restore-session-list') { if(gxOriginalSessionList){host().adapter.listSessions=gxOriginalSessionList;gxOriginalSessionList=null};return {restored:true} }
          if (method === 'gx-enable-multihost-queue') { host().adapter.config.pendingQueueEnabled = true; return {enabled:true} }
          if (method === 'arm-pair-commit-replay-count') {
            const security=host().security, original=security.handlePairingClaim.bind(security), counter={calls:0}
            replayCommitCounter=counter
            security.handlePairingClaim=(input)=>{if(input.claim?.phase==='PAIRING_PHASE_COMMIT') counter.calls++;return original(input)}
            return {armed:true}
          }
          if (method === 'pair-commit-replay-count') return {calls:replayCommitCounter?.calls ?? 0}
          if (method === 'gx-hold-pair-commit-result') {
            const access=host().access, publish=access.publishResult, protocol=codec(), release=Promise.withResolvers()
            const state={entered:false,access,publish,release};gxHeldPairCommitResult=state
            access.publishResult=async function(bytes) { const value=protocol.decode('whalerein.v1.HostResult',bytes); if(value.request_id?.startsWith('pair-commit-') && value.status==='succeeded'){state.entered=true;await release.promise};return publish.call(access,bytes) }
            return {armed:true}
          }
          if (method === 'gx-held-pair-commit-result') return {entered:gxHeldPairCommitResult?.entered===true}
          if (method === 'gx-release-pair-commit-result') { const state=gxHeldPairCommitResult;if(!state)throw new Error('not armed');state.access.publishResult=state.publish;state.release.resolve();gxHeldPairCommitResult=null;return {released:true} }
          if (method === 'gx-revoke-host-grant') { await ctx.get('whalereinSettings').revoke(deviceId); return {revoked:true} }
          if (method === 'channel-revoke-peer') { const result=host().security.forgetPairedDevice(deviceId);return {revoked:Boolean(result?.applied?.includes(deviceId))} }
          if (method === 'queue-native-meta') { const state=await ctx.sessionController.state({sessionId},signal);return {items:(state.queue ?? []).map(item=>({id:item.id,placement:item.placement,rpcId:item.rpcId ?? ''}))} }
          if (method === 'arm-queue-receipt-loss') {
            const adapter=host().adapter
            const original=adapter.updatePendingQueue.bind(adapter)
            adapter.updatePendingQueue=async (...args)=>{adapter.updatePendingQueue=original;await original(...args);throw new Error('synthetic receipt loss')}
            return {armed:true}
          }
          if (method === 'queue-native-meta') { const state=await ctx.sessionController.state({sessionId},signal);return {items:(state.queue ?? []).map(item=>({id:item.id,placement:item.placement,rpcId:item.rpcId ?? ''}))} }
          if (method === 'queue-native-ids') { const state=await ctx.sessionController.state({sessionId},signal);return {ids:(state.queue ?? []).map(item=>item.id)} }
          if (method === 'queue-inject-unknown-block') {
            const original=ctx.sessionController.state.bind(ctx.sessionController)
            const marker=config?.marker
            if(typeof marker !== 'string' || !marker.startsWith('synthetic-')) throw new Error('invalid synthetic marker')
            ctx.sessionController.state=async (...args)=>{
              const state=await original(...args)
              if(args[0]?.sessionId !== sessionId || !Array.isArray(state?.queue) || !state.queue.length) return state
              return {...state,queue:state.queue.map((item,index)=>index ? item : {...item,content:[...item.content,{type:'unknown-future',details:{nested:{arguments:{value:marker}}}}]})}
            }
            return {armed:true}
          }
          if (method === 'queue-add-external') { await ctx.sessionController.prompt({sessionId,requestId:'external-fixture',mode:'queue',content:[{type:'text',text:'synthetic external'}]},signal);return {accepted:true,deliveryId:'external-fixture'} }
          if (method === 'queue-retention-evidence') { const state=await ctx.sessionController.state({sessionId},signal); const events=ctx.sessions.get(sessionId)?.snapshotEvents?.() ?? [];return {available:Array.isArray(state.queue),queued:(state.queue ?? []).map(item=>item.id),committed:events.filter(event=>event.type==='user/message' && typeof event.data?.id==='string').map(event=>event.data.id)} }
          if (method === 'queue-privileged-add') { const agent=ctx.agents.get(sessionId);if (!agent) throw new Error('agent unavailable');const message=createSystemMessage('synthetic privileged content');agent.inbox.append('next-turn',message);return {id:message.id} }
          if (method === 'arm-queue-refresh-failure') {
            const adapter=host().adapter
            const update=adapter.updatePendingQueue.bind(adapter),read=adapter.readPendingQueue.bind(adapter)
            adapter.updatePendingQueue=async (...args)=>{
              const result=await update(...args)
              adapter.updatePendingQueue=update
              adapter.readPendingQueue=async()=>{adapter.readPendingQueue=read;throw new Error('synthetic refresh failure')}
              return result
            }
            return {armed:true}
          }
          if (method === 'queue-history-fingerprint') { const events=ctx.sessions.get(sessionId)?.snapshotEvents?.() ?? [];const rows=events.filter(event=>event.type==='user/message' || event.type==='assistant/message').map(event=>({type:event.type,data:event.data}));return {count:rows.length,digest:createHash('sha256').update(JSON.stringify(rows)).digest('hex')} }
          if (method === 'queue-context-add') {
            const agent = ctx.agents.get(sessionId)
            if (!agent) throw new Error('agent unavailable')
            const message=createUserMessage({content:[{type:'text',text:'synthetic hidden context'}],source:{kind:'time-context',form:'snapshot',sections:[{name:'time-context',text:'synthetic hidden context'}]}})
            agent.inbox.append('next-step',message)
            return {id:message.id}
          }
          if (method === 'queue-invalid-nontext') {
            try { await ctx.sessionController.updateQueue({sessionId,itemId,action:{kind:'edit',content:[{type:'image'}]}},signal);return {rejected:false} }
            catch(error) { return {rejected:error?.code === 'session/attachment-invalid'} }
          }
          if (method === 'queue-edit-direct') { await ctx.sessionController.updateQueue({sessionId,itemId,action:{kind:'edit',content:[{type:'text',text:'later fixture'}]}},signal);return {accepted:true} }
          if (method === 'queue-remove-direct') { await ctx.sessionController.updateQueue({sessionId,itemId,action:{kind:'remove'}},signal);return {accepted:true} }
          if (method === 'queue-add-image') {
            await ctx.sessionController.prompt({sessionId, requestId:'queue-image-fixture', mode:'queue', content:[
              {type:'text',text:'fixture text'},
              {type:'image',mediaType:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGP4z8AARAwQCgAf7gP9i18U1AAAAABJRU5ErkJggg==',name:'fixture.png'},
            ]}, signal)
            return {accepted:true}
          }
          if (method === 'queue-read') {
            host().adapter.config.pendingQueueEnabled = true
            const queue = await host().adapter.readPendingQueue(sessionId)
            return {available:queue.available,running:queue.running,items:queue.items.map(item=>({id:item.item_id,placement:item.placement,actions:item.actions,fixtureText:item.blocks.some(block=>block.text?.text==='replacement fixture'),laterText:item.blocks.some(block=>block.text?.text==='later fixture'),attachments:item.blocks.filter(block=>block.attachment).map(block=>({mediaType:block.attachment.media_type,name:block.attachment.name,size:block.attachment.size,available:block.attachment.available}))}))}
          }
          if (method === 'queue-edit' || method === 'queue-remove' || method === 'queue-promote') {
            host().adapter.config.pendingQueueEnabled = true
            const queue = await host().adapter.readPendingQueue(sessionId)
            const item = queue.items.find(item=>item.actions.includes(method === 'queue-edit' ? 'EDIT' : method === 'queue-remove' ? 'REMOVE' : 'PROMOTE'))
            if (!item) throw new Error('eligible item missing')
            await host().adapter.updatePendingQueue(sessionId,{item_id:item.item_id,action:method === 'queue-edit' ? 'EDIT' : method === 'queue-remove' ? 'REMOVE' : 'PROMOTE',text:'replacement fixture'})
            return {accepted:true,id:item.item_id}
          }
          if (method === 'arm-watch-baseline-barrier') {
            const adapter = host().adapter
            const original = adapter.readState
            let release
            const gate = new Promise(resolve => { release = resolve })
            watchBarrier = { count: 0, release, adapter, original }
            adapter.readState = async function(...args) {
              watchBarrier.count++
              await gate
              return original.apply(adapter, args)
            }
            return {armed:true}
          }
          if (method === 'watch-baseline-barrier-count') return {count:watchBarrier?.count ?? 0}
          if (method === 'release-watch-baseline-barrier') {
            const barrier = watchBarrier
            if (!barrier) throw new Error('barrier not armed')
            barrier.adapter.readState = barrier.original
            barrier.release()
            return {released:true}
          }
          if (method === 'watches') return host().events.describe().watches
          if (method === 'state') {
            const value = await ctx.sessionController.state({ sessionId }, signal)
            return { running: value.running, active: value.active, queueKnown: value.queue !== null, queueCount: value.queue?.length ?? null, pendingInputCount: value.pendingInput?.length ?? 0 }
          }
          if (method === 'adapter-wait') return host().adapter.waitSession(sessionId, timeoutMs, signal)
          if (method === 'adapter-wait-state') return host().adapter.readWaitState(sessionId, signal)
          if (method === 'native-silent-input-refresh') {
            const controller = ctx.sessionController.stateController
            if (!controller || typeof controller.readState !== 'function') throw new Error('native state seam unavailable')
            const original = controller.readState
            const eventsBefore = ctx.sessions.get(sessionId)?.snapshotEvents?.().length
            let reads = 0, changed = false
            controller.readState = async (...args) => {
              const state = await original.apply(controller, args)
              reads++
              if (reads === 1) setTimeout(() => { changed = true }, 100)
              return changed ? { ...state, pendingInput: ['question'] } : state
            }
            const started = performance.now()
            try {
              const result = await host().adapter.waitSession(sessionId, 3500, signal)
              return { reason: result.reason, pending: result.state.pendingInput, reads,
                elapsedMs: performance.now() - started,
                eventsBefore, eventsAfter: ctx.sessions.get(sessionId)?.snapshotEvents?.().length }
            } finally { controller.readState = original }
          }
          if (method === 'wait-remote-question-start') {
            const abort = new AbortController(), gateway = ctx.get('typertGateway')
            const peer = {principal:{kind:'owner'}, interactions:'answer', signal:abort.signal}
            const stream = gateway.openRemoteEvents({args:{}}, abort.signal, peer)
            const first = await stream.next()
            if (first.value?.type !== 'ready') throw new Error('owner stream unavailable')
            const request = {agent:ctx.agents.get(sessionId), questions:[{id:'wait-gateway-fixture', question:'Fixture', options:[{label:'Yes'}]}]}
            const pending = config?.timed ? ctx.userQuestions.askTimed(request, 'wait-expiry-fixture', 2500) : ctx.userQuestions.ask(request)
            pending.catch(() => {})
            const event = await stream.next()
            if (event.value?.type !== 'waterfall') throw new Error('question event unavailable')
            waitRemoteFixture = {abort, stream, pending, gateway, peer, clientId:first.value.clientId, eventId:event.value.eventId}
            return { pending: gateway.getPendingRemoteEvents().filter(item => String(item.agentId) === sessionId).length }
          }
          if (method === 'wait-remote-question-settle') {
            const fixture = waitRemoteFixture
            const response = await fixture.gateway.dispatchRpc('$events/result', {args:{clientId:fixture.clientId,
              eventId:fixture.eventId, outcome:{kind:'result', value:{answers:[{id:'wait-gateway-fixture', selected:['Yes']}]}}}}, signal, fixture.peer)
            const result = await fixture.pending
            return {ok:response.ok, answered:result?.answers?.[0]?.selected?.[0] === 'Yes', pending:fixture.gateway.getPendingRemoteEvents().length}
          }
          if (method === 'wait-remote-question-expired') {
            const result = await waitRemoteFixture.pending
            return {expired:result?.pending === true, pending:waitRemoteFixture.gateway.getPendingRemoteEvents().length}
          }
          if (method === 'wait-remote-question-stop') {
            const fixture = waitRemoteFixture
            waitRemoteFixture = null
            fixture.abort.abort()
            await fixture.stream.return().catch(() => {})
            return { stopped: true }
          }
          if (method === 'native-output-abort') {
            const history = ctx.sessionController.stateController?.historyController
            if (!history || typeof history.assistantOutput !== 'function') throw new Error('native history seam unavailable')
            const original = history.assistantOutput
            const abort = new AbortController()
            let entered = false
            history.assistantOutput = async (...args) => {
              entered = true
              const outputSignal = args.at(-1)
              setTimeout(() => abort.abort(), 50)
              return new Promise((_resolve, reject) => {
                outputSignal.addEventListener('abort', () => reject(outputSignal.reason), { once: true })
                if (outputSignal.aborted) reject(outputSignal.reason)
              })
            }
            try {
              await ctx.sessionController.wait({ sessionId, timeoutMs: 3000 }, abort.signal)
              return { entered, aborted: false }
            } catch (error) {
              if (abort.signal.aborted) return { entered, aborted: true }
              throw error
            } finally { history.assistantOutput = original }
          }
          if (method === 'native-output-failure') {
            const history = ctx.sessionController.stateController?.historyController
            if (!history || typeof history.assistantOutput !== 'function') throw new Error('native history seam unavailable')
            const original = history.assistantOutput
            const marker = new Error('synthetic native output failure')
            history.assistantOutput = async () => { throw marker }
            try {
              await ctx.sessionController.wait({ sessionId, timeoutMs: 3000 }, signal)
              return { propagated: false }
            } catch (error) { return { propagated: error === marker } }
            finally { history.assistantOutput = original }
          }
          if (method === 'arm-native-wait-output-failure') {
            const history = ctx.sessionController.stateController?.historyController
            if (!history || typeof history.assistantOutput !== 'function') throw new Error('native history seam unavailable')
            const original = history.assistantOutput
            history.assistantOutput = async function (...args) {
              history.assistantOutput = original
              throw new Error('synthetic-private-output-marker')
            }
            return { armed: true }
          }
          if (method === 'native-no-first-state') {
            const controller = ctx.sessionController.stateController
            if (!controller || typeof controller.readState !== 'function') throw new Error('native state seam unavailable')
            const original = controller.readState
            controller.readState = async (_id, stateSignal) => new Promise((_resolve, reject) => {
              stateSignal.addEventListener('abort', () => reject(stateSignal.reason), { once: true })
              if (stateSignal.aborted) reject(stateSignal.reason)
            })
            try {
              await ctx.sessionController.wait({ sessionId, timeoutMs: 100 }, signal)
              return { failed: false }
            } catch (error) { return { failed: true, waitTimeout: error?.code === 'session/wait-timeout' } }
            finally { controller.readState = original }
          }
          if (method === 'native-maintenance') {
            const agent = ctx.sessionController.stateController?.ctx?.agents?.get(sessionId)
            if (!agent || typeof agent.runMaintenance !== 'function') throw new Error('native maintenance unavailable')
            let release
            const maintenance = agent.runMaintenance(() => new Promise(resolve => { release = resolve }))
            try {
              const current = await ctx.sessionController.state({ sessionId }, signal)
              let settled = false
              const waiting = ctx.sessionController.wait({ sessionId, timeoutMs: 3000 }, signal)
                .then(result => { settled = true; return result })
              await new Promise(resolve => setTimeout(resolve, 150))
              const premature = settled
              release()
              const result = await waiting
              await maintenance
              return { active: current.active, running: current.running, premature, reason: result.reason }
            } finally { release?.(); await maintenance }
          }
          if (method === 'native-wait' || method === 'native-wait-abort') {
            const abort = new AbortController()
            const timer = method === 'native-wait-abort' ? setTimeout(() => abort.abort(), 100) : null
            try {
              const result = await ctx.sessionController.wait({ sessionId, timeoutMs }, abort.signal)
              return { reason: result.reason, active: result.state.active, running: result.state.running,
                queueAvailable: result.state.queue !== null, pendingInput: result.state.pendingInput }
            } catch (error) {
              if (abort.signal.aborted) return { aborted: true }
              throw error
            } finally { if (timer) clearTimeout(timer) }
          }
          if (method === 'tool-overlap') {
            if (!toolRuns.length) return { runs: [] }
            const origin = Math.min(...toolRuns.map(run => run.started))
            return { runs: toolRuns.map(run => ({ name: run.name, startedMs: run.started - origin,
              finishedMs: run.finished - origin })) }
          }
          if (method === 'native-prompt-turns') {
            const turns = Math.max(1, Number(count ?? 1))
            const signal = AbortSignal.timeout(600000)
            for (let index = 0; index < turns; index++) {
              await ctx.sessionController.prompt({ requestId: `native-cold-${index}`, sessionId, mode: 'queue',
                content: [{ type: 'text', text: `native cold continuation turn ${index}` }] }, signal)
              for (let spin = 0; spin < 200; spin++) {
                const state = await ctx.sessionController.state({ sessionId }, signal)
                if (state.running) break
                await new Promise(resolve => setTimeout(resolve, 5))
              }
              await ctx.sessionController.wait({ sessionId, timeoutMs: 30000 }, signal)
            }
            return { turns, seq: ctx.sessions.get(sessionId)?.seq ?? null }
          }
          if (method === 'send-request-id') {
            return await host().adapter.sendUserMessage(sessionId, 'native send ownership fixture', 'queue', 'native-fixture-request-id')
          }
          if (method === 'events-shape') {
            const events = (await historyEvents(sessionId, { maxMessages })).events
            return { count: events.length, events: events.map(eventShape) }
          }
          if (method === 'stream-shape') {
            return { frames: streamFrames.filter(entry => entry.sessionId === sessionId).slice(-400).map(({ sessionId: _session, ...rest }) => rest) }
          }
          if (method === 'block-assembler-probe') {
            const corrected = new BlockAssembler()
            corrected.push({ type: 'block-start', index: 0, blockType: 'text' })
            corrected.push({ type: 'text-delta', index: 0, text: 'native delta prefix' })
            corrected.push({ type: 'block-end', index: 0, block: { type: 'text', text: 'native corrected block' } })
            corrected.push({ type: 'text-delta', index: 0, text: 'ignored after block end' })
            const ordered = new BlockAssembler()
            ordered.push({ type: 'block-start', index: 0, blockType: 'reasoning' })
            ordered.push({ type: 'block-start', index: 1, blockType: 'text' })
            ordered.push({ type: 'reasoning-delta', index: 0, text: 'native reasoning' })
            ordered.push({ type: 'text-delta', index: 1, text: 'native answer' })
            ordered.push({ type: 'block-end', index: 1, block: { type: 'text', text: 'native answer' } })
            ordered.push({ type: 'block-end', index: 0, block: { type: 'reasoning', text: 'native reasoning' } })
            return { corrected: corrected.blocks().map(blockShape), ordered: ordered.blocks().map(blockShape) }
          }
          if (method === 'native-range') {
            const live = ctx.sessions.get(sessionId)
            if (!live || typeof live.snapshotEvents !== 'function') throw new Error('session not hot')
            const started = Date.now()
            const events = live.snapshotEvents(Math.max(0, Number(fromSeq ?? 0)), toSeq === undefined || toSeq === null ? undefined : Number(toSeq))
            return { records: events.length, bytes: JSON.stringify(events).length,
              firstSeq: events[0]?.seq ?? null, lastSeq: events.at(-1)?.seq ?? null,
              types: events.map(event => event.type), elapsedMs: Date.now() - started }
          }
          if (method === 'native-page') {
            const controller = ctx.sessionController
            if (!controller || typeof controller.events !== 'function') throw new Error('events unavailable')
            const started = Date.now()
            const signal = cancel ? AbortSignal.abort() : AbortSignal.timeout(20000)
            const pageLimit = Math.max(1, Number(maxPages ?? 1))
            let calls = 0, records = 0, stagedBytes = 0, maxPageBytes = 0, pages = 0, minSeq = null, maxSeq = null, hasMore = null, cursor = null
            let before = beforeSeq === undefined || beforeSeq === null ? undefined : Number(beforeSeq)
            try {
              for (let page = 0; page < pageLimit; page++) {
                calls += 1
                const value = await controller.events({ address: { kind: 'session', sessionId },
                  ...(before === undefined ? {} : { beforeSeq: before }),
                  ...(throughSeq === undefined || throughSeq === null ? {} : { throughSeq: Number(throughSeq) }),
                  maxMessages: Math.max(1, Math.min(500, Number(maxMessages ?? 200))) }, signal)
                pages += 1
                records += value.records.length
                const pageBytes = JSON.stringify(value.records).length
                stagedBytes += pageBytes
                maxPageBytes = Math.max(maxPageBytes, pageBytes)
                hasMore = value.hasMore === true
                cursor = value.cursor ?? null
                const seqs = value.records.map(record => record.event?.seq).filter(Number.isSafeInteger)
                if (seqs.length) {
                  minSeq = minSeq === null ? seqs[0] : Math.min(minSeq, seqs[0])
                  maxSeq = maxSeq === null ? seqs[seqs.length - 1] : Math.max(maxSeq, seqs[seqs.length - 1])
                }
                if (!hasMore || !value.records.length) break
                before = value.records[0].event.seq
              }
              return { calls, pages, records, stagedBytes, maxPageBytes, firstSeq: minSeq, lastSeq: maxSeq, minSeq, maxSeq, hasMore, cursor, elapsedMs: Date.now() - started }
            } catch (error) {
              return { calls, pages, records, stagedBytes, maxPageBytes, firstSeq: minSeq, lastSeq: maxSeq, minSeq, maxSeq, hasMore, cursor,
                elapsedMs: Date.now() - started, errorCode: signal.aborted ? 'aborted' : error?.code ?? error?.name ?? 'error' }
            }
          }
          if (method === 'history-materialization') {
            const live = ctx.sessions.get(sessionId)
            if (!live || typeof live.snapshotEvents !== 'function') throw new Error('session not hot')
            const original = live.snapshotEvents.bind(live)
            let calls = 0, materialized = 0
            live.snapshotEvents = (...args) => {
              const value = original(...args)
              calls += 1
              materialized = Math.max(materialized, value.length)
              return value
            }
            const started = Date.now()
            try {
              const snapshot = await host().adapter.getSessionSnapshot(sessionId)
              return { calls, materialized, totalEvents: original().length, complete: snapshot.complete === true,
                cursor: snapshot.cursor ?? null, retainedFrom: snapshot.retained_from ?? null,
                normalized: snapshot.events.length, elapsedMs: Date.now() - started }
            } finally {
              live.snapshotEvents = original
            }
          }
          if (method === 'bulk-append') {
            const session = ctx.sessions.get(sessionId)
            if (!session || typeof session.append !== 'function') throw new Error('session not hot')
            const pairs = Math.max(1, Number(count ?? 1))
            const withTurns = turns === true
            const existingEvents = session.snapshotEvents()
            let lastTurn = -1
            for (const existing of existingEvents) if (existing.type === 'turn/start' && Number.isSafeInteger(existing.data?.turn)) lastTurn = Math.max(lastTurn, existing.data.turn)
            const turnBase = turnOffset === undefined || turnOffset === null ? lastTurn + 1 : Number(turnOffset)
            const started = Date.now()
            let firstSeq = null
            for (let index = 0; index < pairs; index++) {
              const turn = turnBase + index
              if (withTurns) session.append('turn/start', { turn })
              if (withTurns) session.append('step/start', { turn, step: 1 })
              const message = createUserMessage({ content: [{ type: 'text', text: `native history fixture ${index}` }], source: { kind: 'user' } })
              const appended = session.append('user/message', message, { surfaceOp: 'append' })
              if (firstSeq === null) firstSeq = appended.seq
              const reply = createAssistantMessage({ content: [{ type: 'text', text: 'native history reply' }], source: { provider: 'deepseek', model: 'deepseek-flash' } })
              session.append('assistant/message', { turn, step: 1, message: reply, stream: [] }, { surfaceOp: 'append' })
              if (withTurns) {
                session.append('step/end', { turn, step: 1 })
                session.append('turn/end', { turn, reason: { kind: 'completed' } })
              }
            }
            await ctx.sessions.flush(session)
            return { pairs, events: pairs * (withTurns ? 6 : 2), firstSeq, lastSeq: session.seq - 1, elapsedMs: Date.now() - started }
          }
          if (method === 'attachment-occurrence') {
            const wanted = occurrenceKind === 'file' ? 'file' : 'image'
            const found = cold ? await coldOccurrence(sessionId, wanted)
              : { ...occurrenceFrom(ctx.sessions.get(sessionId)?.snapshotEvents?.() ?? [], wanted), source: 'hot' }
            return { ...found, ref: found?.ref ? attachmentKeys(found.ref) : null }
          }
          if (method === 'attachment-read-image') {
            const found = cold ? await coldOccurrence(sessionId, 'image')
              : { ...occurrenceFrom(ctx.sessions.get(sessionId)?.snapshotEvents?.() ?? [], 'image'), source: 'hot' }
            if (!found?.ref) return { available: false, missing: true, source: found?.source ?? null }
            const started = Date.now()
            try {
              const stored = await ctx.get('attachments').readImage(found.ref, AbortSignal.timeout(15000))
              return { available: true, source: found.source, seq: found.seq, ref: attachmentKeys(found.ref),
                readRef: attachmentKeys(stored.ref), dataLen: stored.data.length,
                isUint8Array: stored.data instanceof Uint8Array, digest: sha(Buffer.from(stored.data)),
                elapsedMs: Date.now() - started }
            } catch (error) {
              return { available: false, source: found.source, errorCode: error?.code ?? error?.name ?? 'error' }
            }
          }
          if (method === 'attachment-read-file') {
            const found = cold ? await coldOccurrence(sessionId, 'file')
              : { ...occurrenceFrom(ctx.sessions.get(sessionId)?.snapshotEvents?.() ?? [], 'file'), source: 'hot' }
            if (!found?.ref) return { available: false, missing: true, source: found?.source ?? null }
            const started = Date.now()
            try {
              const value = await readFileAll(found.ref, cancel ? AbortSignal.abort() : AbortSignal.timeout(15000))
              return { available: true, source: found.source, seq: found.seq, ref: attachmentKeys(found.ref), ...value,
                elapsedMs: Date.now() - started }
            } catch (error) {
              return { available: false, source: found.source, cancelled: cancel === true,
                errorCode: error?.code ?? error?.name ?? 'error', elapsedMs: Date.now() - started }
            }
          }
          if (method === 'attachment-file-facts') {
            const empty = await ctx.get('attachments').saveFile({ data: new Uint8Array(0), name: 'native-empty.bin' })
            const payload = new Uint8Array(bytes ?? [110, 97, 116, 105, 118, 101])
            const exact = await ctx.get('attachments').saveFile({ data: payload, name: 'native-exact.bin' })
            const emptyRead = await readFileAll(empty, AbortSignal.timeout(15000))
            const exactRead = await readFileAll(exact, AbortSignal.timeout(15000))
            return { empty: { ref: attachmentKeys(empty), ...emptyRead },
              exact: { ref: attachmentKeys(exact), ...exactRead },
              digestMatches: exactRead.digest === sha(Buffer.from(payload)) }
          }
          if (method === 'attachment-remove-source') {
            const events = ctx.sessions.get(sessionId)?.snapshotEvents?.() ?? []
            const image = occurrenceFrom(events, 'image')
            const file = occurrenceFrom(events, 'file')
            const result = { imageRemoved: false, fileRemoved: false, imageError: null, fileError: null }
            if (image?.ref) {
              const path = ctx.get('attachments').imageHostPath(image.ref)
              if (typeof path === 'string') { fs.unlinkSync(path); result.imageRemoved = true }
              try { await ctx.get('attachments').readImage(image.ref, AbortSignal.timeout(15000)) } catch (error) { result.imageError = error?.code ?? error?.name ?? 'error' }
            }
            if (file?.ref) {
              const path = ctx.get('attachments').fileHostPath(file.ref)
              if (typeof path === 'string') { fs.unlinkSync(path); result.fileRemoved = true }
              try { await readFileAll(file.ref, AbortSignal.timeout(15000)) } catch (error) { result.fileError = error?.code ?? error?.name ?? 'error' }
            }
            return result
          }
          if (method === 'synthetic-source-probe') {
            const session = ctx.sessions.get(sessionId)
            if (!session || typeof session.append !== 'function') throw new Error('session not hot')
            const outcome = {}
            try {
              const unknown = createUserMessage({ content: [{ type: 'text', text: 'native unknown source fixture' }],
                source: { kind: 'fixture-unknown-context', form: 'notice', summary: 'native fixture notice' } })
              const appended = session.append('user/message', unknown, { surfaceOp: 'append' })
              outcome.unknown = { accepted: true, seq: appended.seq }
            } catch (error) { outcome.unknown = { accepted: false, code: error?.code ?? 'rejected' } }
            try {
              const missing = createUserMessage({ content: [{ type: 'text', text: 'native missing source fixture' }], source: {} })
              session.append('user/message', missing, { surfaceOp: 'append' })
              outcome.missing = { accepted: true }
            } catch (error) { outcome.missing = { accepted: false, code: 'rejected' } }
            return outcome
          }
          if (method === 'command-probe') {
            const commands = ctx.get('commands')
            const agent = ctx.agents.get(sessionId)
            if (!commands || typeof commands.execute !== 'function' || !agent) return { available: false }
            const before = (ctx.sessions.get(sessionId)?.snapshotEvents?.() ?? []).length
            let failure, success, failureError = null, successError = null
            try { failure = await commands.execute(agent, '/compact unexpected-fixture-argument', [], AbortSignal.timeout(60000)) }
            catch (error) { failureError = error?.code ?? error?.name ?? 'error' }
            try { success = await commands.execute(agent, '/compact', [], AbortSignal.timeout(60000)) }
            catch (error) { successError = error?.code ?? error?.name ?? 'error' }
            const added = (ctx.sessions.get(sessionId)?.snapshotEvents?.() ?? []).slice(before)
            return { available: true, failureKind: failure?.result?.kind ?? null, successKind: success?.result?.kind ?? null,
              failureError, successError, added: added.map(eventShape) }
          }
          if (method === 'events') {
            const events = ctx.sessions.get(sessionId)?.snapshotEvents?.()
            if (!Array.isArray(events)) throw new Error('session not hot')
            return { types: events.map(event => event.type),
              decisions: events.filter(event => event.type === 'approval/decided').map(event => event.data.outcome) }
          }
          if (method === 'cut-start' || method === 'cut-next') {
            const saved = cuts.get(sessionId)
            if (method === 'cut-next' && !saved) throw new Error('cut missing')
            const page = await host().adapter.readHistory(sessionId, {
              snapshot: true, max_messages: 1, max_bytes: 4096,
              ...(method === 'cut-next' ? { after_cursor: saved.after, cut_cursor: saved.cut } : {}),
            })
            const value = { after: page.batch.to_cursor, cut: page.cut_cursor,
              ids: [...(method === 'cut-next' ? saved.ids : []), ...page.batch.changes.map(c => c.message.message_id)] }
            cuts.set(sessionId, value)
            return { complete: page.complete, ids: value.ids, cut: value.cut }
          }
          throw new Error('unknown fixture operation')
        }).then(result => socket.end(JSON.stringify({ ok: true, result }) + '\n'), () => {
          // Do not export native errors, request contents, config or credentials.
          socket.end(JSON.stringify({ ok: false, code: 'native_operation_failed' }) + '\n')
        })
      })
    })
    server.listen(socketPath, () => fs.chmodSync(socketPath, 0o600))
    return async () => {
      if (questionResolve) questionResolve({ answers: [{ id: 'model-fixture-question', selected: [], custom: 'fixture cleanup' }] })
      removeQuestion?.()
      removeTool?.()
      removeParallelA?.()
      removeParallelB?.()
      removeAttachment?.()
      removeBulk?.()
      await new Promise(resolve => server.close(resolve))
      try { fs.unlinkSync(socketPath) } catch {}
    }
  })
}
