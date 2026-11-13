// Bound, read-only compatibility check for native delivery modes.
// An unverified launcher, runtime or service leaves those modes unavailable.
import fs from 'node:fs'
import path from 'node:path'
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'
import { symbols } from '@deepseek-ai/cordis'

const VERIFIED_VERSION = '0.2.2-rc.1'
const VERIFIED_CORDIS_VERSION = '4.1.0'

function verifiedPlatform() {
  return (process.platform === 'linux' || process.platform === 'darwin') && process.arch === 'arm64' && process.versions.node === '25.9.0'
}

function readManifest(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'))
}

function sameLauncher(binding) {
  if (!verifiedPlatform()) return false
  const launcher = readManifest(binding.launcherManifest)
  const controller = readManifest(binding.controllerManifest)
  const cordis = readManifest(binding.cordisManifest)
  const bin = typeof launcher.bin === 'string' ? launcher.bin : launcher.bin?.dsh
  return launcher.name === '@deepseek-ai/dsh' && launcher.version === VERIFIED_VERSION &&
    controller.name === '@deepseek-ai/dsh-api-session-controller' && controller.version === VERIFIED_VERSION &&
    cordis.name === '@deepseek-ai/cordis' && cordis.version === VERIFIED_CORDIS_VERSION &&
    typeof bin === 'string' &&
    fs.realpathSync(process.argv[1]) === binding.entrypoint &&
    fs.realpathSync(path.resolve(path.dirname(binding.launcherManifest), bin)) === binding.entrypoint &&
    binding.boot.getDshRuntimeVersion() === VERIFIED_VERSION
}

function sameService(ctx, binding) {
  const service = ctx?.sessionController
  const raw = service?.[symbols.original]
  return Boolean(raw && raw === binding.service &&
    ctx instanceof binding.Context && raw.ctx instanceof binding.Context &&
    Object.getPrototypeOf(raw) === binding.Controller.prototype &&
    raw.ctx?.fiber?.runtime?.callback === binding.Controller &&
    !Object.hasOwn(raw, 'prompt') && raw.prompt === binding.prompt &&
    binding.Controller.prototype.prompt === binding.prompt &&
    ctx?.reflect?.props && !ctx.reflect.props['sessionController.prompt'])
}

/** Resolve only an official DSH CLI's verified runtime and active source service. */
export async function resolveDeliveryRuntime(ctx) {
  try {
    if (!verifiedPlatform() || !process.argv[1]) return null
    const entrypoint = fs.realpathSync(process.argv[1])
    const req = createRequire(entrypoint)
    const launcherManifest = req.resolve('@deepseek-ai/dsh/package.json')
    const launcher = readManifest(launcherManifest)
    const bin = typeof launcher.bin === 'string' ? launcher.bin : launcher.bin?.dsh
    if (launcher.name !== '@deepseek-ai/dsh' || launcher.version !== VERIFIED_VERSION ||
        typeof bin !== 'string' || fs.realpathSync(path.resolve(path.dirname(launcherManifest), bin)) !== entrypoint) return null
    const controllerManifest = req.resolve('@deepseek-ai/dsh-api-session-controller/package.json')
    const manifest = readManifest(controllerManifest)
    if (manifest.name !== '@deepseek-ai/dsh-api-session-controller' || manifest.version !== VERIFIED_VERSION) return null
    const cordisManifest = req.resolve('@deepseek-ai/cordis/package.json')
    const cordis = readManifest(cordisManifest)
    if (cordis.name !== '@deepseek-ai/cordis' || cordis.version !== VERIFIED_CORDIS_VERSION) return null
    const boot = await import(pathToFileURL(req.resolve('@deepseek-ai/dsh-app-boot')).href)
    const { Context } = await import(pathToFileURL(req.resolve('@deepseek-ai/cordis')).href)
    const { SessionController } = await import(pathToFileURL(req.resolve('@deepseek-ai/dsh-api-session-controller')).href)
    if (typeof boot.getDshRuntimeVersion !== 'function' || typeof Context !== 'function' || typeof SessionController !== 'function') return null
    const service = ctx?.sessionController?.[symbols.original]
    const binding = {entrypoint, launcherManifest, controllerManifest, cordisManifest, boot, Context, Controller: SessionController,
      service, prompt: SessionController.prototype.prompt}
    return isDeliveryRuntimeCurrent(ctx, binding) ? binding : null
  } catch {
    return null
  }
}

/** Check the binding again immediately before advertising or invoking delivery. */
export function isDeliveryRuntimeCurrent(ctx, binding) {
  if (!binding) return false
  try { return sameLauncher(binding) && sameService(ctx, binding) }
  catch { return false }
}
