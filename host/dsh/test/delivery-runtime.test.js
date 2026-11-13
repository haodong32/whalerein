import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { symbols } from '@deepseek-ai/cordis'
import { isDeliveryRuntimeCurrent, resolveDeliveryRuntime } from '../src/adapter/delivery-runtime.js'
import { RealDshAdapter } from '../src/adapter/real.js'

function useRuntime(t, platform = 'linux', arch = 'arm64', node = '25.9.0') {
  const values = [[process, 'platform', platform], [process, 'arch', arch], [process.versions, 'node', node]]
  const originals = values.map(([target, name]) => [target, name, Object.getOwnPropertyDescriptor(target, name)])
  for (const [target, name, value] of values) {
    const descriptor = Object.getOwnPropertyDescriptor(target, name)
    Object.defineProperty(target, name, {...descriptor, value})
  }
  t.after(() => { for (const [target, name, descriptor] of originals) Object.defineProperty(target, name, descriptor) })
}

async function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'whalerein-delivery-runtime-'))
  const scope = path.join(root, 'node_modules', '@deepseek-ai')
  const launcher = path.join(scope, 'dsh')
  const boot = path.join(scope, 'dsh-app-boot')
  const controller = path.join(scope, 'dsh-api-session-controller')
  const cordis = path.join(scope, 'cordis')
  for (const dir of [path.join(launcher, 'lib'), boot, controller, cordis]) fs.mkdirSync(dir, {recursive: true})
  const writeJson = (file, value) => fs.writeFileSync(file, JSON.stringify(value))
  const launcherManifest = path.join(launcher, 'package.json')
  const controllerManifest = path.join(controller, 'package.json')
  const bootManifest = path.join(boot, 'package.json')
  const cordisManifest = path.join(cordis, 'package.json')
  const launcherValue = {name:'@deepseek-ai/dsh',version:'0.2.2-rc.1',bin:{dsh:'lib/bin.js'},exports:{'./package.json':'./package.json'}}
  const controllerValue = {name:'@deepseek-ai/dsh-api-session-controller',version:'0.2.2-rc.1',type:'module',exports:{'.':'./index.mjs','./package.json':'./package.json'}}
  writeJson(launcherManifest, launcherValue)
  writeJson(controllerManifest, controllerValue)
  writeJson(bootManifest, {name:'@deepseek-ai/dsh-app-boot',version:'0.2.2-rc.1',type:'module',exports:{'.':'./index.mjs'}})
  const cordisValue = {name:'@deepseek-ai/cordis',version:'4.1.0',type:'module',exports:{'.':'./index.mjs','./package.json':'./package.json'}}
  writeJson(cordisManifest, cordisValue)
  const entry = path.join(launcher, 'lib', 'bin.js')
  fs.writeFileSync(entry, '')
  fs.writeFileSync(path.join(boot, 'index.mjs'), "import fs from 'node:fs'; export function getDshRuntimeVersion() { return JSON.parse(fs.readFileSync(new URL('./package.json', import.meta.url), 'utf8')).version }\n")
  fs.writeFileSync(path.join(controller, 'index.mjs'), 'export class SessionController { constructor() { this.calls = 0 } async prompt() { this.calls++; return {accepted:true} } }\n')
  fs.writeFileSync(path.join(cordis, 'index.mjs'), 'export class Context {}\n')
  const {Context} = await import(pathToFileURL(path.join(cordis, 'index.mjs')).href)
  const {SessionController} = await import(pathToFileURL(path.join(controller, 'index.mjs')).href)
  const raw = new SessionController()
  raw.ctx = Object.assign(new Context(), {fiber:{runtime:{callback:SessionController}}})
  const service = new Proxy(raw, {get(target, key, receiver) { return key === symbols.original ? target : Reflect.get(target, key, receiver) }})
  const ctx = Object.assign(new Context(), {sessionController: service, reflect: {props: {}}})
  return {root, entry, launcherManifest, launcherValue, controllerManifest, controllerValue, cordisManifest, cordisValue, Context, bootManifest, raw, service, ctx, writeJson}
}

test('verified standard runtime is rechecked for version, launcher, service and method changes without gating production delivery', async t => {
  useRuntime(t)
  const f = await fixture()
  const previousArgv = process.argv[1]
  process.argv[1] = f.entry
  try {
    const proof = await resolveDeliveryRuntime(f.ctx)
    assert.ok(proof)
    assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), true)
    const adapter = new RealDshAdapter({ctx:f.ctx, config:{}})
    await adapter.open()
    try {
      assert.deepEqual(adapter.supportedDeliveryModes(), ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER'])
      const accepted = await adapter.execute('s1', {send_message:{text:'fixture'}}, {operationId:'accepted'})
      assert.equal(accepted.command_accepted.delivery_id, 'accepted')
      assert.equal(f.raw.calls, 1)
      adapter.deliveryModes = ['START_IF_IDLE', 'STEER']
      await assert.rejects(adapter.execute('s1', {send_message:{text:'fixture'}}), error => error.code === 'unsupported_capability')
      assert.equal(f.raw.calls, 1)
      const steered = await adapter.execute('s1', {send_message:{text:'fixture', delivery_mode:'STEER'}}, {operationId:'steered'})
      assert.equal(steered.command_accepted.delivery_id, 'steered')
      assert.equal(f.raw.calls, 2)
      adapter.deliveryModes = ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN']
      await assert.rejects(adapter.execute('s1', {send_message:{text:'fixture', delivery_mode:'STEER'}}), error => error.code === 'unsupported_capability')
      assert.equal(f.raw.calls, 2)
      const queued = await adapter.execute('s1', {send_message:{text:'fixture'}}, {operationId:'queued'})
      assert.equal(queued.command_accepted.delivery_id, 'queued')
      assert.equal(f.raw.calls, 3)
      // Delivery entry points stay open in production even when the runtime
      // compatibility probe no longer matches the active CLI installation.
      adapter.deliveryModes = ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER']
      const alternate = {...f.ctx, sessionController: new Proxy({}, {get(_target, key) { return key === symbols.original ? {} : async () => ({accepted:true}) }})}
      assert.equal(isDeliveryRuntimeCurrent(alternate, proof), false)
      const original = f.raw.prompt
      f.raw.prompt = async () => ({accepted:true})
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), false)
      assert.deepEqual(adapter.supportedDeliveryModes(), ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER'])
      delete f.raw.prompt
      assert.equal(f.raw.prompt, original)
      f.ctx.reflect.props['sessionController.prompt'] = {}
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), false)
      delete f.ctx.reflect.props['sessionController.prompt']
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), true)
      f.writeJson(f.launcherManifest, {...f.launcherValue, version:'0.2.2-rc.2'})
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), false)
      assert.equal(await resolveDeliveryRuntime(f.ctx), null)
      f.writeJson(f.launcherManifest, f.launcherValue)
      f.writeJson(f.controllerManifest, {...f.controllerValue, version:'0.2.2-rc.2'})
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), false)
      f.writeJson(f.controllerManifest, f.controllerValue)
      f.writeJson(f.cordisManifest, {...f.cordisValue, version:'4.1.1'})
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), false)
      assert.equal(await resolveDeliveryRuntime(f.ctx), null)
      f.writeJson(f.cordisManifest, f.cordisValue)
      f.writeJson(f.bootManifest, {name:'@deepseek-ai/dsh-app-boot',version:'0.2.2-rc.2',type:'module',exports:{'.':'./index.mjs'}})
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), false)
      fs.unlinkSync(f.bootManifest)
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), false)
      process.argv[1] = path.join(f.root, 'wrapper.js')
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), false)
      assert.equal(await resolveDeliveryRuntime(f.ctx), null)
    } finally { await adapter.close() }
  } finally {
    process.argv[1] = previousArgv
    fs.rmSync(f.root, {recursive:true, force:true})
  }
})

test('missing launcher or source identity stays unavailable', async t => {
  useRuntime(t)
  const f = await fixture()
  const previousArgv = process.argv[1]
  process.argv[1] = f.entry
  try {
    assert.equal(await resolveDeliveryRuntime({}), null)
    assert.equal(await resolveDeliveryRuntime({...f.ctx, reflect: undefined}), null)
    f.writeJson(f.launcherManifest, {...f.launcherValue, bin:{dsh:'lib/another.js'}})
    assert.equal(await resolveDeliveryRuntime(f.ctx), null)
  } finally {
    process.argv[1] = previousArgv
    fs.rmSync(f.root, {recursive:true, force:true})
  }
})


test('a physical copy of the same controller version cannot impersonate the injected service', async t => {
  useRuntime(t)
  const f = await fixture()
  const previousArgv = process.argv[1]
  process.argv[1] = f.entry
  try {
    const original = await resolveDeliveryRuntime(f.ctx)
    assert.ok(original)
    const copy = path.join(f.root, 'controller-copy')
    fs.cpSync(path.dirname(f.controllerManifest), copy, {recursive:true})
    const {SessionController} = await import(pathToFileURL(path.join(copy, 'index.mjs')).href)
    const raw = new SessionController()
    raw.ctx = Object.assign(new f.Context(), {fiber:{runtime:{callback:SessionController}}})
    const service = new Proxy(raw, {get(target, key, receiver) { return key === symbols.original ? target : Reflect.get(target, key, receiver) }})
    const alternate = Object.assign(new f.Context(), {sessionController:service, reflect:f.ctx.reflect})
    assert.equal(isDeliveryRuntimeCurrent(alternate, original), false)
    assert.equal(await resolveDeliveryRuntime(alternate), null)
  } finally {
    process.argv[1] = previousArgv
    fs.rmSync(f.root, {recursive:true, force:true})
  }
})

test('a standard CLI symlink outside the installation resolves the active DSH package', async t => {
  useRuntime(t)
  const f = await fixture()
  const previousArgv = process.argv[1]
  const bin = fs.mkdtempSync(path.join(os.tmpdir(), 'whalerein-external-bin-'))
  const link = path.join(bin, 'dsh')
  fs.symlinkSync(f.entry, link)
  process.argv[1] = link
  try {
    const proof = await resolveDeliveryRuntime(f.ctx)
    assert.ok(proof)
    assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), true)
  } finally {
    process.argv[1] = previousArgv
    fs.rmSync(bin, {recursive:true, force:true})
    fs.rmSync(f.root, {recursive:true, force:true})
  }
})


test('only the verified platform, architecture and Node version open delivery modes', async t => {
  useRuntime(t)
  const f = await fixture()
  const previousArgv = process.argv[1]
  process.argv[1] = f.entry
  try {
    const proof = await resolveDeliveryRuntime(f.ctx)
    assert.ok(proof)
    for (const [target, name, value] of [
      [process, 'platform', 'freebsd'],
      [process, 'platform', 'win32'],
      [process, 'arch', 'x64'],
      [process.versions, 'node', '25.8.0'],
      [process.versions, 'node', '25.9.1'],
    ]) {
      const original = Object.getOwnPropertyDescriptor(target, name)
      Object.defineProperty(target, name, {...original, value})
      try {
        assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), false)
        assert.equal(await resolveDeliveryRuntime(f.ctx), null)
      } finally { Object.defineProperty(target, name, original) }
    }
    assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), true)
    const platform = Object.getOwnPropertyDescriptor(process, 'platform')
    Object.defineProperty(process, 'platform', {...platform, value:'darwin'})
    try {
      assert.equal(isDeliveryRuntimeCurrent(f.ctx, proof), true)
      assert.ok(await resolveDeliveryRuntime(f.ctx))
    } finally { Object.defineProperty(process, 'platform', platform) }
  } finally {
    process.argv[1] = previousArgv
    fs.rmSync(f.root, {recursive:true, force:true})
  }
})
