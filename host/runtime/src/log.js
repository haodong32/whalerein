// Structured diagnostic logging. Every identifier is replaced by a local
// salted HMAC except for temporary operation trace IDs. Only an allowlist
// of scalar fields is emitted so a caller
// cannot accidentally log message bodies, tool arguments, keys, or ciphertext.

import { createHmac, randomBytes } from 'node:crypto'

const LEVELS = { debug: 10, info: 20, warn: 30, error: 40, silent: 100 }

/** Fields that may appear in a log record. Everything else is dropped. */
const ID_FIELDS = new Set([
  'request', 'command', 'host', 'device', 'session', 'channel', 'approval', 'replay',
])
const SCALAR_FIELDS = new Set([
  'status', 'code', 'kind', 'phase', 'eventSeq', 'count', 'depth', 'ms', 'bytes',
  'limit', 'mode', 'online', 'reconnects', 'cursor', 'source', 'complete', 'pending', 'layer', 'stage', 'scope', 'reason', 'reasonCode', 'generation', 'revokedAt', 'final',
])

export function createLogSalt() {
  return randomBytes(16).toString('hex')
}

export function redactId(salt, value) {
  if (value === undefined || value === null || value === '') return ''
  return createHmac('sha256', Buffer.from(salt, 'hex')).update(String(value)).digest('hex').slice(0, 12)
}

function scalar(value) {
  if (typeof value === 'number') return Number.isFinite(value) ? value : 0
  if (typeof value === 'boolean') return value
  if (typeof value === 'string') return value.replace(/[^\x20-\x7e]/g, '').slice(0, 64)
  return undefined
}

/**
 * @param {object} options
 * @param {string} options.salt local random salt for identifier HMACs.
 * @param {string} [options.level] debug, info, warn, error, silent.
 * @param {(record: object) => void} [options.sink] record sink; defaults to one JSON line on stderr.
 * @param {() => number} [options.now] clock injection for tests.
 */
export function createLogger({ salt, level = 'info', sink, now = Date.now } = {}) {
  const threshold = LEVELS[level] ?? LEVELS.info
  const write = sink ?? ((record) => process.stderr.write(`${JSON.stringify(record)}\n`))
  const emit = (levelName, event, fields = {}) => {
    if (LEVELS[levelName] < threshold) return
    const record = { time: now(), level: levelName, event }
    for (const [key, value] of Object.entries(fields)) {
      // Operation IDs are ephemeral and must match Access without local hashing.
      if (key === 'trace' && typeof value === 'string' && /^[A-Za-z0-9_-]{1,64}$/.test(value)) {
        record.trace_id = value
        continue
      }
      if (ID_FIELDS.has(key)) {
        const redacted = redactId(salt ?? '', value)
        if (redacted) record[key] = redacted
        continue
      }
      if (!SCALAR_FIELDS.has(key)) continue
      const clean = scalar(value)
      if (clean !== undefined) record[key] = clean
    }
    write(record)
  }
  return {
    debug: (event, fields) => emit('debug', event, fields),
    info: (event, fields) => emit('info', event, fields),
    warn: (event, fields) => emit('warn', event, fields),
    error: (event, fields) => emit('error', event, fields),
    level,
  }
}

export const silentLogger = createLogger({ salt: '0'.repeat(32), level: 'silent' })
