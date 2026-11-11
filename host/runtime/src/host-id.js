// One host-id generator for debug seeding and production startup.
// The result is an MQTT topic id: a slug of the computer name plus a short suffix.

import { randomBytes } from 'node:crypto'

export const HOST_ID_ALPHABET = 'abcdefghjkmnpqrstuvwxyz23456789'
const MAX_HOST_ID_LENGTH = 128

/** Topic-safe slug of a computer name. An empty result becomes `host`. */
export function slugHostName(hostname) {
  let text = String(hostname ?? '').trim().toLowerCase()
  if (text.endsWith('.local')) text = text.slice(0, -'.local'.length)
  text = text.replace(/[^a-z0-9_.:-]+/g, '-').replace(/-+/g, '-').replace(/^-+|-+$/g, '')
  return text || 'host'
}

/**
 * Build a host id. `random` returns 4 bytes used only to pick the suffix.
 * @param {string} hostname
 * @param {(size: number) => Uint8Array} [random]
 */
export function generateHostId(hostname, random = randomBytes) {
  const alphabet = HOST_ID_ALPHABET
  const sample = random(4)
  let suffix = ''
  for (let index = 0; index < 4; index += 1) suffix += alphabet[sample[index] % alphabet.length]
  const room = MAX_HOST_ID_LENGTH - suffix.length - 1
  let name = slugHostName(hostname)
  if ([...name].length > room) name = [...name].slice(0, room).join('').replace(/-+$/g, '') || 'host'
  return `${name}-${suffix}`
}
