// Test SourceAdapter with stored attachment bytes and exact membership. It is
// intentionally DSH-free: Host transfer ownership must not depend on connector
// internals.
import { createHash } from 'node:crypto'
import { TestSource } from './source.js'
import { AdapterError } from '../../src/errors.js'

const keyOf = (sessionId, ref) => `${sessionId}\u0000${ref}`

export class AttachmentTestSource extends TestSource {
  blobs = new Map()
  memberships = new Map()
  opens = 0
  nativeReads = 0
  nativeBytes = 0
  closeCalls = []
  openFailure = null
  oversize = 0
  truncateTo = null
  readHook = null
  readDelayMs = 0

  constructor({
    now = Date.now,
    sessions = [{ sessionId: 's1', running: false }],
    policy = {
      max_total_bytes: 4096,
      max_chunk_bytes: 4,
      max_concurrent: 4,
      idle_timeout_ms: 30_000,
      overall_timeout_ms: 120_000,
    },
  } = {}) {
    super({ now, sessions })
    this.policy = { ...policy }
  }

  attach({
    sessionId = 's1', ref, bytes, name = 'note.bin', mediaType = 'application/octet-stream',
    sha256 = null, missing = false, sessionHostId = 'host-1',
  }) {
    const data = Buffer.from(bytes)
    let refs = this.memberships.get(sessionId)
    if (!refs) { refs = new Set(); this.memberships.set(sessionId, refs) }
    refs.add(ref)
    this.blobs.set(keyOf(sessionId, ref), {
      sessionId, ref, data, name, mediaType, sha256, missing, sessionHostId,
    })
    return this
  }

  async openAttachment(sessionId, ref, { signal } = {}) {
    this.opens++
    if (signal?.aborted) throw new AdapterError('unavailable')
    if (this.openFailure) throw new AdapterError(this.openFailure)
    if (!this.sessions.has(sessionId)) throw new AdapterError('not_found')
    if (!this.memberships.get(sessionId)?.has(ref)) throw new AdapterError('not_found')
    const blob = this.blobs.get(keyOf(sessionId, ref))
    if (!blob || blob.missing) throw new AdapterError('not_found')
    return {
      attachment: {
        session: { host_id: blob.sessionHostId, session_id: sessionId },
        attachment_ref: ref,
        media_type: blob.mediaType,
        name: blob.name,
        size: blob.data.length,
        ...(blob.sha256 ? { sha256: new Uint8Array(blob.sha256) } : {}),
      },
      policy: { ...this.policy },
      reader: {
        read: async (maxBytes, { signal, offset } = {}) => {
          this.nativeReads++
          if (this.readHook) await this.readHook({ sessionId, ref, offset, maxBytes, signal })
          if (this.readDelayMs > 0) await new Promise(resolve => setTimeout(resolve, this.readDelayMs))
          if (signal?.aborted) throw new AdapterError(signal.reason === 'expired' ? 'expired' : 'unavailable')
          const limit = this.truncateTo === null ? blob.data.length : Math.min(this.truncateTo, blob.data.length)
          if (offset >= limit) return { bytes: new Uint8Array(0), eof: true }
          const end = Math.min(limit, offset + maxBytes)
          let chunk = blob.data.subarray(offset, end)
          if (this.oversize > 0) chunk = blob.data.subarray(offset, Math.min(blob.data.length, end + this.oversize))
          this.nativeBytes += chunk.length
          return { bytes: new Uint8Array(chunk), eof: end >= limit }
        },
        close: async ({ cancel } = {}) => { this.closeCalls.push({ sessionId, ref, cancel }) },
      },
    }
  }

  digestFor(sessionId, ref) {
    return createHash('sha256').update(this.blobs.get(keyOf(sessionId, ref)).data).digest()
  }
}
