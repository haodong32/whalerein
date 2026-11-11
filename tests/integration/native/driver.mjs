// Test-only native setup and observations. Never part of the published connector.
import net from 'node:net'
import fs from 'node:fs'
import { createRequire } from 'node:module'
const installed = createRequire('/opt/native/node_modules/@deepseek-ai/dsh/package.json')
const { defineTool } = await import(installed.resolve('@deepseek-ai/dsh-tools'))

export const name = 'whalerein-native-driver'
export const inject = ['sessionController', 'sessions', 'settings', 'pluginManager', 'tools', 'approval', 'workspaceController']

export function apply(ctx) {
  const socketPath = process.env.WHALEREIN_NATIVE_SOCKET
  ctx.effect(() => {
    try { fs.unlinkSync(socketPath) } catch {}
    const cuts = new Map()
    let effects = 0
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
    const host = () => ctx.get('whalereinSettings')?.host
    const server = net.createServer(socket => {
      let buffer = ''
      socket.on('data', chunk => {
        buffer += chunk
        if (buffer.length > 65536) { socket.destroy(); return }
        if (!buffer.includes('\n')) return
        const line = buffer.slice(0, buffer.indexOf('\n'))
        buffer = ''
        Promise.resolve().then(async () => {
          const { method, sessionId, config } = JSON.parse(line)
          const signal = AbortSignal.timeout(15000)
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
          if (method === 'create') return ctx.sessionController.create({ cwd: process.cwd() })
          if (method === 'start') return host().adapter.sendUserMessage(sessionId, 'native integration fixture')
          if (method === 'watches') return host().events.describe().watches
          if (method === 'state') {
            const value = await ctx.sessionController.state({ sessionId }, signal)
            return { running: value.running, active: value.active }
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
      removeTool?.()
      await new Promise(resolve => server.close(resolve))
      try { fs.unlinkSync(socketPath) } catch {}
    }
  })
}
