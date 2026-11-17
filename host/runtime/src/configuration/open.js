// Configuration open: one file, read-only inspection by default, and atomic
// default initialization only for an ordinary fresh startup.
import { readFileSync } from 'node:fs'
import { dirname } from 'node:path'
import { canonicalTarget, selectConfigPath } from './paths.js'
import { parseConfiguration } from './document.js'
import { revisionOf } from './revision.js'
import { atomicReplace } from './io.js'
import { configError } from './errors.js'

export const INITIAL_CONFIGURATION = 'version = 1\nenvironment = "production"\n'

function legacyStateDetected(options, directory) {
  const probe = options.legacyState
  if (typeof probe === 'function') {
    try {
      return Boolean(probe(directory))
    } catch {
      // A failing legacy probe means state may exist; never overwrite it.
      return true
    }
  }
  return probe === true
}

/** Build a snapshot from already-read text. Used by open and by update. */
function decodeUtf8(bytes) {
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes)
  } catch {
    throw configError('invalid_syntax', 'configuration is not valid UTF-8 text')
  }
}

export function snapshotFromText(requestedPath, target, bytes, { kind = 'explicit', inspectionOnly = false, initialized = false, homeDir } = {}) {
  const document = parseConfiguration(decodeUtf8(bytes))
  return Object.freeze({
    source: Object.freeze({ path: target, kind, initialized }),
    path: target,
    target,
    requestedPath,
    sourceKind: kind,
    revision: revisionOf(bytes),
    exists: true,
    inspectionOnly,
    initialized,
    document,
    ...(homeDir === undefined ? {} : { homeDir }),
  })
}

/**
 * Open the selected configuration file.
 *
 * options:
 * - configFile: explicit path (wins over the environment variable)
 * - env: false disables WHALEREIN_CONFIG
 * - envVars / homeDir: test and sandbox injection
 * - inspectionOnly: never initialize or write, even when the default is missing
 * - allowDefaultInit: ordinary startup may atomically create the default file
 * - legacyState: boolean or probe(directory); truthy state suppresses initialization
 * - io: fault-injection hooks for the initializing write
 */
export function open(options = {}) {
  const selected = selectConfigPath(options)

  let bytes
  try {
    bytes = readFileSync(selected.path)
  } catch (error) {
    if (error?.code !== 'ENOENT') {
      throw configError('unreadable_config', 'the configuration file exists but cannot be read', { path: selected.path })
    }
    if (selected.kind !== 'default' || options.allowDefaultInit !== true || options.inspectionOnly === true) {
      throw configError('missing_config', 'the selected configuration file does not exist', { path: selected.path })
    }
    if (legacyStateDetected(options, dirname(selected.path))) {
      throw configError('missing_config', 'legacy state was detected; import or explicitly set up configuration before a new default is created', { path: selected.path })
    }
    atomicReplace(selected.path, Buffer.from(INITIAL_CONFIGURATION, 'utf8'), { io: options.io, hooks: options.hooks })
    try {
      bytes = readFileSync(selected.path)
    } catch {
      throw configError('unreadable_config', 'the initialized configuration file could not be read', { path: selected.path })
    }
    return snapshotFromText(selected.path, canonicalTarget(selected.path), bytes, {
      kind: selected.kind,
      initialized: true,
      homeDir: options.homeDir,
    })
  }

  return snapshotFromText(selected.path, canonicalTarget(selected.path), bytes, {
    kind: selected.kind,
    inspectionOnly: options.inspectionOnly === true,
    homeDir: options.homeDir,
  })
}
