// Test-only provenance for the native integration runtime.
//
// Native cases boot a real DSH process that loads the connector from the
// mounted worktree (`/workspace`). A copied or cached bundle would make every
// native result describe code nobody edited, so the runtime reports which
// files it actually loaded and the harness compares their content digests with
// the mounted sources before a case runs.
import fs from 'node:fs'
import path from 'node:path'
import { createHash } from 'node:crypto'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

export const WORKSPACE = '/workspace'
export const RUNTIME_SOURCE = path.join(WORKSPACE, 'host/runtime/src')
export const DSH_SOURCE = path.join(WORKSPACE, 'host/dsh/src')

const SKIPPED_DIRECTORIES = new Set(['node_modules', '.git'])

/**
 * Digest a source tree exactly like `native_environment.source_digest`.
 *
 * Each regular file contributes `sha256(relativePath + NUL + bytes)`; the
 * sorted contribution digests are hashed again. The order is byte-stable, so
 * Python and Node can compare one value without shipping file lists around.
 */
export function treeDigest(root) {
  const digests = []
  const walk = (directory) => {
    for (const name of fs.readdirSync(directory).sort()) {
      if (SKIPPED_DIRECTORIES.has(name)) continue
      const full = path.join(directory, name)
      const stat = fs.lstatSync(full)
      if (stat.isSymbolicLink()) continue
      if (stat.isDirectory()) {
        walk(full)
        continue
      }
      if (!stat.isFile()) continue
      const relative = path.relative(root, full).split(path.sep).join('/')
      digests.push(
        createHash('sha256')
          .update(relative)
          .update('\0')
          .update(fs.readFileSync(full))
          .digest('hex'),
      )
    }
  }
  walk(root)
  digests.sort()
  return createHash('sha256').update(digests.join('')).digest('hex')
}

function resolvedEntry(specifier, anchor) {
  try {
    const requireFromDsh = createRequire(anchor)
    const resolved = requireFromDsh.resolve(specifier)
    const real = fs.realpathSync(resolved)
    return { specifier, resolved, real }
  } catch (error) {
    return { specifier, error: String(error?.message ?? error) }
  }
}

function dshVersion() {
  try {
    return JSON.parse(fs.readFileSync(path.join('/opt/native/node_modules/@deepseek-ai/dsh/package.json'), 'utf8')).version
  } catch {
    return null
  }
}

/**
 * Describe the code this process actually loaded.
 *
 * `insideWorkspace` is the decisive field: a plugin-manager copy, a registry
 * install, or a stale image layer resolves outside `/workspace` and fails the
 * harness check instead of silently testing other bytes.
 */
export function collectProvenance() {
  const driverEntry = fileURLToPath(import.meta.url)
  const hostRuntime = resolvedEntry('whalerein-host', path.join(WORKSPACE, 'host/dsh/package.json'))
  const adapterEntry = path.join(DSH_SOURCE, 'adapter/real.js')
  return {
    version: 1,
    workspace: WORKSPACE,
    computedAt: Date.now(),
    driverEntry: {
      path: driverEntry,
      real: fs.realpathSync(driverEntry),
      insideWorkspace: fs.realpathSync(driverEntry).startsWith(path.join(WORKSPACE, 'tests/integration/native') + path.sep),
    },
    hostRuntime: {
      ...hostRuntime,
      real: hostRuntime.real ?? null,
      insideWorkspace: Boolean(hostRuntime.real && hostRuntime.real.startsWith(RUNTIME_SOURCE + path.sep)),
    },
    dshAdapter: {
      path: adapterEntry,
      real: fs.realpathSync(adapterEntry),
      insideWorkspace: fs.realpathSync(adapterEntry).startsWith(DSH_SOURCE + path.sep),
    },
    hashes: {
      'host/runtime/src': treeDigest(RUNTIME_SOURCE),
      'host/dsh/src': treeDigest(DSH_SOURCE),
    },
    versions: { node: process.version, dsh: dshVersion() },
  }
}