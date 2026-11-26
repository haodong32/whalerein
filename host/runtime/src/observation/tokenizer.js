// Run-local opaque business tokens.
//
// A trusted boundary replaces a raw identifier with an HMAC-SHA256 token
// derived from the ephemeral capture key and a domain separator. The same raw
// identity maps to the same token for the run, and the token reveals neither
// the identity nor its domain. Raw identifiers never enter an observation
// record.
import { createHmac } from 'node:crypto'

export const TOKEN_DOMAINS = Object.freeze([
  'session',
  'stream',
  'attempt',
  'block',
  'frame',
  'request',
  'watch',
  'connection',
  'object',
  'batch',
])

const DOMAIN_SET = new Set(TOKEN_DOMAINS)
// A short token keeps records small while remaining collision-resistant at the
// bounded cardinality of one capture. The wire form is 16 bytes as 32 hex
// characters, shared by Rust, Go and Node producers.
const TOKEN_BYTES = 16
export const TOKEN_VERSION = 'whalerein/latency-token/v1'

function keyBytes(key) {
  if (key instanceof Uint8Array) {
    if (key.length < 16) throw new TypeError('capture key must be at least 16 bytes')
    return key
  }
  if (typeof key === 'string' && /^[0-9a-fA-F]{64}$/.test(key)) {
    return Buffer.from(key, 'hex')
  }
  if (typeof key === 'string' && key.length >= 16) return Buffer.from(key, 'utf8')
  throw new TypeError('capture key must be at least 16 bytes')
}

/**
 * @param {Uint8Array|string} key ephemeral capture key.
 * @returns {{token:(domain:string, raw:unknown)=>string}}
 */
export function createTokenizer(key) {
  const secret = keyBytes(key)
  function token(domain, raw) {
    if (!DOMAIN_SET.has(domain)) throw new TypeError(`unknown token domain ${String(domain)}`)
    if (raw === undefined || raw === null || raw === '') return ''
    if (typeof raw === 'number') {
      if (!Number.isSafeInteger(raw) || raw < 0) throw new TypeError('token identity must be a positive integer or string')
      raw = String(raw)
    }
    if (typeof raw !== 'string') throw new TypeError('token identity must be a string')
    return createHmac('sha256', secret)
      .update(TOKEN_VERSION)
      .update('\u0000')
      .update(domain)
      .update('\u0000')
      .update(raw, 'utf8')
      .digest('hex')
      .slice(0, TOKEN_BYTES * 2)
  }
  return Object.freeze({ token })
}

/**
 * Record fields that carry a business identity and their token domain. A
 * producer passes raw ids through the outlet; only tokens reach the file.
 *
 * `batch` is deliberately absent: its raw identity is the composite
 * `<stream>:<seq_start>:<seq_end>`, so a caller derives it with `batchToken`
 * from values it already holds. Tokenizing it here would hash the caller's
 * token a second time and break every join.
 */
const TOKEN_FIELDS = Object.freeze({
  stream: 'stream',
  attempt: 'attempt',
  block: 'block',
  request: 'request',
  connection: 'connection',
})

/**
 * Replace every identity field with its run-local token. `attempt` is a
 * transport ordinal on `host_publish`, so the event name disambiguates that
 * one overloaded field.
 */
export function tokenizeData(data, tokenizer) {
  if (!data || typeof data !== 'object') return data
  const out = { ...data }
  for (const [field, domain] of Object.entries(TOKEN_FIELDS)) {
    if (out[field] === undefined) continue
    // A numeric ordinal (for example host_publish.attempt) is a transport
    // value, not an opaque identity, and is never tokenized.
    if (typeof out[field] === 'number') continue
    try {
      out[field] = tokenizer.token(domain, out[field])
    } catch {
      // An invalid identity is not partially exported: the whole event is
      // rejected by the manifest validator below.
      out[field] = ''
    }
  }
  return out
}