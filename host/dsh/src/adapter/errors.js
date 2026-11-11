import { AdapterError, PluginError } from 'whalerein-host/errors.js'

/**
 * Map an arbitrary adapter rejection to a stable wire code. The raw DSH error
 * text is never returned to the caller, logged, or written to storage.
 */
export function mapAdapterError(error) {
  if (error instanceof AdapterError) return error.code
  if (error instanceof PluginError) return error.code
  const rawText = error && typeof error === 'object'
    ? `${error.code ?? ''} ${error.name ?? ''} ${error.message ?? ''}`.toLowerCase()
    : String(error ?? '').toLowerCase()
  // DSH remote failures use slash/hyphen codes such as `session/not-found`.
  const raw = `${rawText} ${rawText.replace(/[/-]/g, '_')}`
  if (raw.includes('not found') || raw.includes('not_found') || raw.includes('session_not_found')) {
    return 'not_found'
  }
  if (raw.includes('unsupported') || raw.includes('not implemented')) return 'unsupported_capability'
  if (raw.includes('abort') || raw.includes('cancel')) return 'unavailable'
  if (raw.includes('busy') || raw.includes('conflict') || raw.includes('writer-held')) return 'conflict'
  if (raw.includes('expired') || raw.includes('timeout')) return 'expired'
  if (raw.includes('unauthorized') || raw.includes('not authenticated')) return 'unauthorized'
  if (raw.includes('forbidden') || raw.includes('denied')) return 'forbidden'
  if (raw.includes('credentials') || raw.includes('unavailable')) return 'unavailable'
  if (raw.includes('invalid')) return 'invalid_argument'
  return 'internal'
}
