// One-file path resolution and one shared home-expansion rule.
import { realpathSync } from 'node:fs'
import { homedir } from 'node:os'
import { join, resolve } from 'node:path'
import { configError } from './errors.js'

export const DEFAULT_CONFIG_DIR = '.whalerein'
export const DEFAULT_CONFIG_FILE = 'config.toml'

/** Expand a leading `~` exactly once; never run a shell or read env values. */
export function expandHome(value, home = homedir()) {
  if (typeof value !== 'string') return value
  if (value === '~') return home
  if (value.startsWith('~/') || value.startsWith('~\\')) return join(home, value.slice(2))
  return value
}

export function defaultConfigPath(home = homedir()) {
  return join(home, DEFAULT_CONFIG_DIR, DEFAULT_CONFIG_FILE)
}

/**
 * Resolve the single configuration source.
 * explicit config path -> WHALEREIN_CONFIG (unless `env: false`) -> default.
 */
export function selectConfigPath(options = {}) {
  const home = options.homeDir ?? homedir()
  const env = options.envVars ?? process.env
  const explicit = options.configFile
  if (explicit !== undefined && explicit !== null) {
    if (typeof explicit !== 'string' || explicit.length === 0 || explicit.includes('\0')) {
      throw configError('missing_config', 'configuration path is not a usable file path')
    }
    return { kind: 'explicit', path: resolve(explicit) }
  }
  if (options.env !== false) {
    const fromEnv = env.WHALEREIN_CONFIG
    if (typeof fromEnv === 'string' && fromEnv.length > 0) {
      if (fromEnv.includes('\0')) throw configError('missing_config', 'WHALEREIN_CONFIG is not a usable file path')
      return { kind: 'environment', path: resolve(fromEnv) }
    }
  }
  return { kind: 'default', path: defaultConfigPath(home) }
}

/**
 * Resolve the canonical target once. A symlink that later points elsewhere is
 * a changed target, which the writer reports as a revision conflict.
 */
export function canonicalTarget(path) {
  try {
    return realpathSync(path)
  } catch {
    return resolve(path)
  }
}
