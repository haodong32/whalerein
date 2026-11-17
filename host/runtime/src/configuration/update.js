// Revisioned settings update: lock, reread, compare, patch, validate, replace.
import { readFileSync } from 'node:fs'
import { applyPatch } from './editor.js'
import { parseConfiguration } from './document.js'
import { atomicReplace, lockPathFor, withConfigLock } from './io.js'
import { canonicalTarget } from './paths.js'
import { revisionOf } from './revision.js'
import { snapshotFromText } from './open.js'
import { configError } from './errors.js'

function conflict(message, details, draft) {
  throw configError('revision_conflict', message, { draft, ...details })
}

function liveRevision(path) {
  try {
    return revisionOf(readFileSync(path))
  } catch {
    return null
  }
}

/**
 * Apply a typed patch to the configuration snapshot.
 *
 * `expectedRevision` is the revision the caller observed (`snapshot.revision`).
 * The writer rechecks the canonical target and content revision under the SQLite
 * lock; a changed external edit returns `revision_conflict` with the caller's
 * draft intact and never overwrites the newer file.
 */
export function update(snapshot, patch, expectedRevision, options = {}) {
  if (!snapshot || typeof snapshot !== 'object') throw configError('missing_config', 'configuration snapshot is required')
  const expected = expectedRevision === undefined ? snapshot.revision : expectedRevision
  const requestedPath = snapshot.requestedPath ?? snapshot.path

  return withConfigLock(
    lockPathFor(snapshot.path),
    () => {
      const liveTarget = canonicalTarget(requestedPath)
      if (liveTarget !== snapshot.path) {
        conflict('the configuration target changed; reread and retry', { currentRevision: liveRevision(liveTarget), expectedRevision: expected }, patch)
      }

      let bytes = null
      try {
        bytes = readFileSync(snapshot.path)
      } catch (error) {
        if (error?.code !== 'ENOENT') {
          throw configError('unreadable_config', 'the configuration file exists but cannot be read', { path: snapshot.path })
        }
      }
      const currentRevision = bytes === null ? null : revisionOf(bytes)
      if (currentRevision !== expected) {
        conflict('configuration changed before commit; reread and retry', { currentRevision, expectedRevision: expected }, patch)
      }
      if (bytes === null) throw configError('missing_config', 'the configuration file does not exist', { path: snapshot.path })

      const baseText = bytes.toString('utf8')
      const currentDocument = parseConfiguration(baseText)
      const nextText = applyPatch(baseText, patch, currentDocument)
      // Validate before any temporary file or rename: an invalid patch leaves
      // the existing file byte-identical.
      parseConfiguration(nextText)
      const nextBytes = Buffer.from(nextText, 'utf8')
      atomicReplace(snapshot.path, nextBytes, { io: options.io, hooks: options.hooks })
      return snapshotFromText(requestedPath, snapshot.path, nextBytes, {
        kind: snapshot.sourceKind,
        initialized: false,
        homeDir: snapshot.homeDir,
      })
    },
    { currentRevision: snapshot.revision },
  )
}
