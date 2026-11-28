// Harness-neutral SourceAdapter fixture. Messages are already normalized;
// native DSH history, projection and approval races belong to connector tests.
import { SourceAdapter } from '../../src/contracts/source.js'
import { AdapterError, DefinitiveExecutionError } from '../../src/errors.js'
import { approvalDigest, randomId } from '../../src/security/digest.js'

export class TestSource extends SourceAdapter {
  sessions = new Map()
  messages = []
  approvals = new Map()
  watchers = new Map()
  failures = new Map()

  constructor({ now = Date.now, sessions = [] } = {}) {
    super()
    this.now = now
    for (const session of sessions) this.sessions.set(session.sessionId, { agentAvailable: true, ...session })
  }

  describe() {
    return { mode: 'fixture', available: true, capabilities: ['sessions', 'events', 'messages', 'interrupt', 'approvals', 'snapshot', 'sync'] }
  }

  async close() { this.watchers.clear() }

  failNext(method, code) { this.failures.set(method, code) }

  check(method, ErrorType = AdapterError) {
    const code = this.failures.get(method)
    this.failures.delete(method)
    if (code) throw new ErrorType(code)
  }

  async listSessions() {
    this.check('listSessions')
    return [...this.sessions.values()]
  }

  async getSession(id) {
    const session = this.sessions.get(id)
    if (!session) throw new DefinitiveExecutionError('not_found')
    return session
  }

  setRunning(id, running) { this.sessions.get(id).running = running }

  watch(id, sink) {
    const sinks = this.watchers.get(id) ?? new Set()
    this.watchers.set(id, sinks)
    sinks.add(sink)
    return { close: () => { sinks.delete(sink); if (!sinks.size) this.watchers.delete(id) } }
  }

  publish(id, update) {
    for (const sink of this.watchers.get(id) ?? []) sink({ session: { session_id: id }, ...update })
  }

  async readState(id) {
    const session = await this.getSession(id)
    return { session: { session_id: id }, history_head: `bookmark:${this.messages.length}`, live_epoch: 'fixture-epoch', revision: 1, run_phase: session.running ? 'RUNNING' : 'IDLE', approvals: [] }
  }

  async readHistory(id) {
    await this.getSession(id)
    const cursor = `bookmark:${this.messages.length}`
    return { session: { session_id: id }, batch: { from_cursor: 'bookmark:start', to_cursor: cursor, batch_id: randomId(), changes: this.messages.filter(m => m.sessionId === id).map(({ message }) => ({ message })) }, cut_cursor: cursor, complete: true }
  }

  async sendMessage(id, text) {
    this.check('sendMessage', DefinitiveExecutionError)
    const message = { message_id: randomId(), role: 'USER', blocks: [{ block_id: randomId(), text: { text } }] }
    const from = `bookmark:${this.messages.length}`
    this.messages.push({ sessionId: id, message })
    this.publish(id, { history: { from_cursor: from, to_cursor: `bookmark:${this.messages.length}`, batch_id: randomId(), changes: [{ message }] } })
    return { command_accepted: { message_id: message.message_id } }
  }

  async interrupt(id) { this.setRunning(id, false); return {} }

  requestApproval({
    sessionId,
    ttlMs = 300_000,
    reason = '',
    toolName = 'fixture',
    reviewAction = '',
    reviewTarget = '',
    reviewImpact = '',
    reviewAvailability = 'unavailable',
  }) {
    const approval = {
      approvalId: randomId(),
      sessionId,
      toolName,
      callId: randomId(),
      reason,
      reviewAction,
      reviewTarget,
      reviewImpact,
      reviewAvailability,
      version: 1,
      expiresAt: this.now() + ttlMs,
      state: 'pending',
      decision: 'unspecified',
    }
    approval.digest = approvalDigest(approval)
    this.approvals.set(approval.approvalId, approval)
    return { ...approval }
  }

  async getApproval(id) {
    const approval = this.approvals.get(id)
    if (approval?.state === 'pending' && approval.expiresAt <= this.now()) approval.state = 'expired'
    return approval ? { ...approval } : null
  }

  async listApprovals(id) {
    const approvals = await Promise.all([...this.approvals.keys()].map(key => this.getApproval(key)))
    return approvals.filter(a => (!id || a.sessionId === id) && a.state === 'pending')
  }

  async simulateResolution(id, decision) { this.approvals.get(id).state = decision === 'approve' ? 'approved' : 'rejected' }

  async execute(id, command) {
    const session = await this.getSession(id)
    if (command.send_message) {
      if (session.running) throw new DefinitiveExecutionError('conflict')
      return this.sendMessage(id, command.send_message.text)
    }
    if (command.interrupt) return this.interrupt(id)
    if (command.resolve_approval) {
      if (!session.running) throw new DefinitiveExecutionError('approval_unavailable')
      const resolve = command.resolve_approval
      await this.simulateResolution(resolve.approval_id, resolve.decision === 'APPROVAL_DECISION_APPROVE' ? 'approve' : 'reject')
      return {}
    }
    throw new DefinitiveExecutionError('unsupported_capability')
  }
}
