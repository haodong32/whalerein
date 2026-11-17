// Scoped TLS trust. No global Node TLS mutation, no shell, no install action.
//
// system -> platform roots (no explicit CA)
// mkcert -> fixed per-platform mkcert rootCA.pem discovery (built-in local only)
// file   -> explicit CA resolved against the configuration parent, `~` once
import { existsSync, readFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { dirname, isAbsolute, join, resolve } from 'node:path'
import { Agent, fetch as undiciFetch } from 'undici'
import { configError } from './errors.js'
import { expandHome } from './paths.js'
import { resolutionContext } from './resolve.js'

const TLS_ERROR_PATTERN = /cert|self.signed|unable.to.verify|altname|issuer|tls|ssl|handshake/i

/** Fixed, shell-free mkcert CA discovery for each supported platform. */
export function discoverMkcertCa(options = {}) {
  const platform = options.platform ?? process.platform
  const home = options.homeDir ?? homedir()
  const env = options.envVars ?? process.env
  if (platform === 'darwin') return join(home, 'Library', 'Application Support', 'mkcert', 'rootCA.pem')
  if (platform === 'win32') {
    const localAppData = env.LOCALAPPDATA
    const base = typeof localAppData === 'string' && localAppData.length > 0
      ? localAppData
      : join(home, 'AppData', 'Local')
    return join(base, 'mkcert', 'rootCA.pem')
  }
  if (platform === 'linux') {
    const xdg = env.XDG_DATA_HOME
    const base = typeof xdg === 'string' && xdg.length > 0 ? xdg : join(home, '.local', 'share')
    return join(base, 'mkcert', 'rootCA.pem')
  }
  throw configError('invalid_tls', 'mkcert trust is not supported on this platform; configure an explicit ca_file')
}

function readCaFile(path, environment) {
  let bytes
  try {
    bytes = readFileSync(path)
  } catch {
    throw configError('invalid_tls', `CA file is missing or unreadable (${path}); install mkcert or set ca_file`, { environment })
  }
  if (!bytes.includes('BEGIN CERTIFICATE')) {
    throw configError('invalid_tls', `CA file does not contain a certificate (${path}); correct ca_file`, { environment })
  }
  return bytes
}

/**
 * Resolve the selected environment's trust into CA material scoped to that
 * environment. A missing/wrong CA is a typed error and never changes selection.
 */
export function resolveTrust(resolution, options = {}) {
  if (!resolution || typeof resolution !== 'object' || typeof resolution.environment !== 'string') {
    throw configError('invalid_tls', 'an effective environment is required to resolve TLS trust')
  }
  const environment = resolution.environment
  const context = resolutionContext(resolution)
  const snapshot = context?.snapshot ?? options.snapshot
  const tls = resolution.definition?.tls ?? { ca_provider: 'system', ca_file: null }
  const provider = tls.ca_provider ?? 'system'

  if (provider === 'system') return { provider: 'system', environment, caFile: null, ca: null, scope: environment }

  if (provider === 'mkcert') {
    if (environment !== 'local') {
      throw configError('invalid_tls', 'mkcert trust is only available for the built-in local environment', { environment })
    }
    const caFile = discoverMkcertCa(options)
    if (!existsSync(caFile)) {
      throw configError('invalid_tls', `mkcert CA is missing (${caFile}); install mkcert or set ca_file for local`, { environment })
    }
    return { provider: 'mkcert', environment, caFile, ca: readCaFile(caFile, environment), scope: environment }
  }

  if (provider === 'file') {
    if (typeof tls.ca_file !== 'string' || tls.ca_file.length === 0) {
      throw configError('invalid_tls', 'ca_file is required for file trust', { environment })
    }
    const configPath = snapshot?.path ?? options.configPath
    if (typeof configPath !== 'string') throw configError('invalid_tls', 'configuration path is required to resolve a relative ca_file', { environment })
    const expanded = expandHome(tls.ca_file, options.homeDir ?? snapshot?.homeDir ?? homedir())
    const caFile = isAbsolute(expanded) ? expanded : resolve(dirname(configPath), expanded)
    return { provider: 'file', environment, caFile, ca: readCaFile(caFile, environment), scope: environment }
  }

  throw configError('invalid_tls', `unknown ca_provider ${provider}`, { environment })
}

function isTlsVerificationError(error) {
  const text = `${error?.code ?? ''} ${error?.cause?.code ?? ''} ${error?.message ?? ''}`
  return TLS_ERROR_PATTERN.test(text)
}

/**
 * Scoped undici Agent/fetch factory. The CA is applied only through
 * `connect.ca` on a per-environment Agent; process-global TLS settings and the
 * default global dispatcher are untouched. Callers close the client explicitly.
 */
export function createScopedClient(resolution, options = {}) {
  const trust = resolveTrust(resolution, options)
  const AgentClass = options.Agent ?? Agent
  const agent = trust.ca ? new AgentClass({ connect: { ca: trust.ca } }) : new AgentClass()
  const fetchImpl = options.fetch ?? undiciFetch
  let closed = false

  return {
    trust,
    agent,
    async fetch(url, init = {}) {
      if (closed) throw configError('invalid_tls', 'TLS trust client is closed', { environment: trust.environment })
      try {
        return await fetchImpl(url, { ...init, dispatcher: agent })
      } catch (error) {
        if (isTlsVerificationError(error)) {
          throw configError('invalid_tls', 'TLS certificate verification failed for the selected environment; verify ca_provider/ca_file and hostname', { environment: trust.environment })
        }
        throw error
      }
    },
    async close() {
      if (closed) return
      closed = true
      await agent.close()
    },
  }
}
