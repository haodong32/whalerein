// Content revision shared by both runtimes: sha256 over the exact file bytes.
import { createHash } from 'node:crypto'

export function revisionOf(bytes) {
  const value = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes)
  return `sha256:${createHash('sha256').update(value).digest('hex')}`
}
