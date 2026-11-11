// Adapter factory. `auto` selects the real adapter only when the Cordis host
// actually exposes DSH session services; standalone runs fall back to FakeDSH.

import { FakeDshAdapter } from './fake.js'
import { RealDshAdapter } from './real.js'

export function hasDshServices(ctx) {
  if (!ctx || typeof ctx !== 'object') return false
  try {
    return Boolean(ctx.sessionController || ctx.sessions)
  } catch {
    return false
  }
}

/**
 * @param {object} options
 * @param {'auto'|'fake'|'real'} [options.mode]
 * @param {object} [options.ctx]
 * @param {object} [options.config]
 * @param {object} [options.store]
 * @param {object} [options.logger]
 * @param {() => number} [options.now]
 * @param {object} [options.adapter] explicit adapter instance (tests/harness).
 */
export function createAdapter({ mode = 'auto', ctx, config, store, logger, now, adapter } = {}) {
  if (adapter) return adapter
  if (mode === 'fake') {
    return new FakeDshAdapter({ store, logger, now })
  }
  if (mode === 'real') {
    return new RealDshAdapter({ ctx, config, store, logger, now })
  }
  if (hasDshServices(ctx)) return new RealDshAdapter({ ctx, config, store, logger, now })
  return new FakeDshAdapter({ store, logger, now })
}

export { DshAdapter, normalizeEvent, normalizeSummary, normalizeApproval, EVENT_KINDS, CONTROL_EVENT_KINDS } from './base.js'
export { FakeDshAdapter } from './fake.js'
export { RealDshAdapter } from './real.js'
