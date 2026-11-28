// One host-id generator for debug seeding and production startup.
//
// The public route identifier is opaque, stable and independent of the Host
// display name or OS. A slug of the computer name is only a display-name
// prefill for the add-Host preview; it must never become a route, topic or
// directory identifier.

import { randomBytes } from 'node:crypto'

export const HOST_ID_ALPHABET = 'abcdefghjkmnpqrstuvwxyz23456789'
export const HOST_ID_PREFIX = 'host-'
export const HOST_ID_RANDOM_LENGTH = 20
const MAX_HOST_ID_LENGTH = 128
const LEGACY_SUFFIX_LENGTH = 4

/** Topic-safe slug of a computer name. A label only, never an identifier. */
export function slugHostName(hostname) {
  let text = String(hostname ?? '').trim().toLowerCase()
  if (text.endsWith('.local')) text = text.slice(0, -'.local'.length)
  text = text.replace(/[^a-z0-9_.:-]+/g, '-').replace(/-+/g, '-').replace(/^-+|-+$/g, '')
  return text || 'host'
}

/**
 * Build an opaque host id from high-entropy random bytes.
 *
 * `random` is injectable for tests only; production uses the OS CSPRNG.
 * @param {(size: number) => Uint8Array} [random]
 */
export function generateHostId(random = randomBytes) {
  const sample = random(HOST_ID_RANDOM_LENGTH)
  let id = HOST_ID_PREFIX
  for (let index = 0; index < HOST_ID_RANDOM_LENGTH; index += 1) {
    id += HOST_ID_ALPHABET[sample[index] % HOST_ID_ALPHABET.length]
  }
  return id.slice(0, MAX_HOST_ID_LENGTH)
}

/**
 * True for the retired `<computer-name-slug>-<four characters>` route format.
 *
 * Only an id derived from the current computer name is treated as legacy.
 * Opaque ids, managed profile ids and explicit operator ids stay untouched.
 */
export function isNameDerivedHostId(hostId, hostname) {
  const value = String(hostId ?? '')
  if (!value || value.startsWith(HOST_ID_PREFIX)) return false
  const match = /^(.*)-([a-z2-9]{4})$/.exec(value)
  if (!match) return false
  const [prefix] = match.slice(1)
  if (!prefix) return false
  const slug = slugHostName(hostname)
  if (prefix === slug) return true
  // The retired generator truncated long names to the id length bound.
  return prefix.length >= 8 && slug.startsWith(prefix)
}