// Shared-contract client environment configuration for the Node Host runtime.
// This is the Host-side equivalent of the Rust Core configuration module: it
// parses the same catalog/corpus, follows the same validation order and writer
// protocol, and never invokes Core or a CLI process.
export { CONFIG_ERROR_CODES, ConfigurationError, configError, redactError } from './errors.js'
export { loadCatalog, BUILTIN_ENVIRONMENTS, isBuiltinEnvironment } from './catalog.js'
export {
  IDENTIFIER_PATTERN,
  parseConfiguration,
  parseToml,
  normalizeTls,
  emptyConfiguration,
} from './document.js'
export { open, INITIAL_CONFIGURATION } from './open.js'
export { resolveEnvironment } from './resolve.js'
export { update } from './update.js'
export { view } from './view.js'
export {
  discoverMkcertCa,
  resolveTrust,
  createScopedClient,
} from './tls.js'
export { revisionOf } from './revision.js'
