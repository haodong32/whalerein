import assert from 'node:assert/strict'
import test from 'node:test'
import { Context, Service } from '@deepseek-ai/cordis'

let loaded
globalThis.window = {
  __ModuleLoader__: {
    load(module) {
      loaded = module
    },
  },
}
await import('../client.js')

// Opaque host component handles: these tests inspect the plugin's composition
// and callbacks, not a reimplementation of DSH rendering or focus behavior.
const UI = Object.fromEntries([
  'Button', 'DisclosureRow', 'Menu', 'SettingsForm', 'Input', 'StateDot', 'Tag', 'IconCheckCircleOutlineRegular',
  'IconWarningOutlineRegular', 'IconLinkOutlineRegular', 'IconChevronDownOutlineRegular',
  'IconSettingsOutlineRegular', 'IconSlidersTwoOutlineRegular', 'IconCodeOutlineRegular',
].map((name) => [name, `DSH.${name}`]))
const primitives = {
  ...UI,
  writeClipboard: async (value) => {
    try { await globalThis.navigator.clipboard.writeText(value); return true } catch { return false }
  },
}
function requireClient(React) {
  return (name) => {
    if (name === 'react') return React
    assert.equal(name, '@deepseek-ai/dsh-client-ui-primitives')
    return primitives
  }
}

function text(node) {
  if (node === null || node === undefined || typeof node === 'boolean') return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(text).join('')
  if (typeof node.type === 'function') return text(node.type(node.props))
  const copy = node.type === UI.DisclosureRow ? node.props.title

    : ''
  return (copy ?? '') + text(node.props?.headerAccessory) + text(node.props?.anchor) + text(node.children)
}

function find(node, predicate) {
  if (node === null || node === undefined) return undefined
  if (Array.isArray(node)) {
    for (const item of node) {
      const found = find(item, predicate)
      if (found !== undefined) return found
    }
    return undefined
  }
  if (typeof node === 'object') {
    if (predicate(node)) return node
    if (typeof node.type === 'function') return find(node.type(node.props), predicate)
    const accessory = find([node.props?.headerAccessory, node.props?.anchor], predicate)
    if (accessory !== undefined) return accessory
    const found = find(node.children, predicate)
    if (found !== undefined) return found
  }
  return undefined
}

function findAll(node, predicate, found = []) {
  if (node === null || node === undefined) return found
  if (Array.isArray(node)) {
    for (const item of node) findAll(item, predicate, found)
    return found
  }
  if (typeof node === 'object') {
    if (predicate(node)) found.push(node)
    if (typeof node.type === 'function') findAll(node.type(node.props), predicate, found)
    findAll([node.props?.headerAccessory, node.props?.anchor, node.children], predicate, found)
  }
  return found
}

function createReactRuntime() {
  // A tiny React-shaped renderer with per-component hook frames. Components
  // are resolved into a plain element tree so assertions observe the same tree
  // a browser would commit, and effects run for nested components too.
  const frames = new Map()
  let current = null
  const React = {
    createElement(type, props, ...children) {
      return { type, props: props ?? {}, children }
    },
    useId() { return 'whalerein-test' },
    useRef(value) {
      const frame = current
      const index = frame.refIndex++
      if (!(index in frame.refs)) frame.refs[index] = { current: value }
      return frame.refs[index]
    },
    useState(initial) {
      const frame = current
      const index = frame.stateIndex++
      if (!(index in frame.state)) frame.state[index] = typeof initial === 'function' ? initial() : initial
      return [frame.state[index], (value) => {
        frame.state[index] = typeof value === 'function' ? value(frame.state[index]) : value
      }]
    },
    useEffect(callback, deps) {
      const frame = current
      const index = frame.effectIndex++
      const previous = frame.effects[index]
      const changed = !previous || !deps || deps.length !== previous.deps?.length
        || deps.some((value, i) => !Object.is(value, previous.deps[i]))
      if (changed) frame.pending.push({ index, callback, deps })
    },
  }
  function frameFor(key) {
    let frame = frames.get(key)
    if (!frame) {
      frame = { state: [], refs: [], effects: [], pending: [], stateIndex: 0, refIndex: 0, effectIndex: 0 }
      frames.set(key, frame)
    }
    return frame
  }
  function callComponent(type, props, key) {
    const frame = frameFor(key)
    const previous = current
    current = frame
    frame.stateIndex = 0
    frame.refIndex = 0
    frame.effectIndex = 0
    frame.pending = []
    try {
      const output = type(props)
      for (const effect of frame.pending) {
        const prior = frame.effects[effect.index]
        prior?.cleanup?.()
        frame.effects[effect.index] = { deps: effect.deps, cleanup: effect.callback() }
      }
      return output
    } finally {
      current = previous
    }
  }
  function resolve(node, path) {
    if (node === null || node === undefined || typeof node !== 'object') return node
    if (Array.isArray(node)) return node.map((item, index) => resolve(item, `${path}.${index}`))
    if (typeof node.type === 'function') {
      const key = `${node.type.name || 'Component'}#${path}`
      return resolve(callComponent(node.type, node.props, key), `${path}>`)
    }
    return {
      type: node.type,
      props: {
        ...node.props,
        anchor: resolve(node.props?.anchor, `${path}.anchor`),
        headerAccessory: resolve(node.props?.headerAccessory, `${path}.accessory`),
      },
      children: resolve(node.children, `${path}.children`),
    }
  }
  function render(component, props) {
    return resolve({ type: component, props: props ?? {}, children: [] }, 'root')
  }
  async function dispose() {
    for (const frame of frames.values()) {
      for (const effect of frame.effects) await effect?.cleanup?.()
    }
    frames.clear()
  }
  return { React, render, dispose }
}

const SECRET_MASK = '••••••••'

function savedSecrets() {
  return {
    ok: true,
    value: {
      namespaces: [
        null,
        { ns: 'other', secrets: [{ path: ['admin_token'], set: true }] },
        {
          ns: 'whalerein-dsh',
          secrets: [
            null,
            { path: ['admin_token'], set: true },
            { path: ['host_token'], set: true },
            { path: ['host_token', 'extra'], set: true },
            { path: 'host_token', set: true },
            { set: true },
            { path: ['ca_file'], set: false },
          ],
        },
      ],
    },
  }
}

async function createSettingsRuntime({
  connected = true, development = false, remoteFailure = false, describe = savedSecrets,
  writable = true, managed = false,
} = {}) {
  const writes = []
  let currentRemoteFailure = remoteFailure
  const disposers = []
  const registered = []
  const developerListeners = new Set()
  const localeListeners = new Set()
  let developerMode = development
  let locales
  const revoked = []
  let revokeResult = { ok: true, value: { deviceId: 'device-phone' } }
  let inviteResult = {
    ok: true,
    value: {
      pairingUri: 'whalerein://pair?v=1&host=host-real&token=once',
      expiresAtUnixMs: Date.now() + 600_000,
      fingerprint: 'abcd ef01 2345 6789',
    },
  }
  let currentStatus = managed ? {
    available: true,
    managed: true,
    state: 'idle',
    environment: 'production',
    profileId: 'profile-production',
    label: 'WhaleRein Cloud',
    hostName: 'Studio Mac',
    stagingEnabled: false,
    developerToolsEnabled: false,
    approvalUrl: '',
    error: null,
    connected: false,
    pendingOperation: false,
    profiles: [
      { id: 'profile-production', label: 'WhaleRein Cloud', environment: 'production', active: true, managed: true },
    ],
    capabilities: [],
    configError: null,
  } : {
    available: true,
    connected,
    hostId: 'host-real',
    pairingCode: 'abcd ef01 2345 6789',
    diagnostics: { identityReady: true, dshAvailable: true, revocationsPending: true,
      reconnects: 2, publishErrors: 1, pendingCiphertext: 3, unknownCommands: 4, snapshotRequired: 5 },
    devices: [
      { deviceId: 'device-phone', displayName: '手机', os: 'ios', fingerprint: 'f1', pairedAt: 1700000000000, connected: true },
      { deviceId: 'device-laptop', fingerprint: 'f2', pairedAt: 1700000001000, connected: false },
    ],
  }
  let formSnapshot = {
    status: 'ready',
    value: {
      mqtt_url: 'mqtt://127.0.0.1:1883',
      host_id: 'host-real',
      host_token: 'hidden',
      ca_file: '',
      state_dir: '/tmp/whalerein',
      adapter: 'real',
      log_level: 'debug',
      allow_insecure_mqtt: true,
    },
    user: { mqtt_url: 'mqtt://127.0.0.1:1883' },
    base: {},
    writable,
    revision: 3,
  }
  let mutateResult = true
  const mutations = []
  const formListeners = new Set()
  const form = {
    getSnapshot: () => formSnapshot,
    subscribe(listener) {
      formListeners.add(listener)
      return () => formListeners.delete(listener)
    },
    set: async (key, value) => {
      writes.push(['set', key, value])
      return true
    },
    unset: async (key) => {
      writes.push(['unset', key])
      return true
    },
    mutate: async (ops, revision) => {
      mutations.push({ ops, revision })
      if (mutateResult !== true) return false
      for (const op of ops) writes.push(op.op === 'set' ? ['set', op.path[0], op.value] : ['unset', op.path[0]])
      return true
    },
  }
  const developerTools = {
    enabled: {
      getSnapshot: () => developerMode,
      subscribe(listener) {
        developerListeners.add(listener)
        return () => developerListeners.delete(listener)
      },
    },
  }
  const react = createReactRuntime()
  const calls = []
  const actionResults = {}
  const managerAction = (name) => async (...args) => {
    calls.push([name, ...args])
    const result = actionResults[name]
    if (result instanceof Error) throw result
    return result ?? { ok: true, value: managed ? currentStatus : {} }
  }
  const remote = {
    $mount: async (contribution) => {
      registered.push({ contribution })
      return async () => {}
    },
    whalerein: {
      snapshot: async () => currentRemoteFailure
        ? { ok: false, error: { code: 'gateway/internal' } }
        : { ok: true, value: currentStatus },
      invite: async () => {
        if (inviteResult instanceof Error) throw inviteResult
        return inviteResult
      },
      revoke: async (deviceId) => {
        revoked.push(deviceId)
        if (revokeResult instanceof Error) throw revokeResult
        return revokeResult
      },
      details: managerAction('details'),
      startConnect: managerAction('startConnect'),
      cancel: managerAction('cancel'),
      retry: managerAction('retry'),
      disconnect: managerAction('disconnect'),
      reconnect: managerAction('reconnect'),
      removeComputer: managerAction('removeComputer'),
      switchProfile: managerAction('switchProfile'),
      enableStaging: managerAction('enableStaging'),
      addCustomService: managerAction('addCustomService'),
      deleteProfile: managerAction('deleteProfile'),
    },
    settings: {
      describe: async () => describe(),
    },
  }
  const plugin = loaded.factory(requireClient(react.React))
  const ctx = {
    locale: {
      bind: () => (key, params) => {
        const template = locales?.dictionaries.zh[key] ?? key
        return params ? template.replace(/\{(\w+)\}/g, (_, name) => String(params[name])) : template
      },
      register(namespace, dictionaries) {
        locales = { namespace, dictionaries }
        return () => {}
      },
      getSnapshot: () => ({ active: 'zh' }),
      subscribe(listener) {
        localeListeners.add(listener)
        return () => localeListeners.delete(listener)
      },
    },
    configForms: {
      developerTools,
      get(entry) {
        assert.equal(entry, 'whalerein-dsh')
        return form
      },
    },
    remote,
    slots: {
      inject(name, register) {
        assert.ok(['settings.section', 'plugins.row.config'].includes(name))
        registered.push(register())
        return () => {}
      },
      register(options, Component) {
        const entry = { options, Component }
        registered.push(entry)
        return () => {}
      },
    },
    effect(callback) {
      const dispose = callback()
      if (typeof dispose === 'function') disposers.push(dispose)
      return () => {}
    },
    inject(names, callback) {
      assert.deepEqual(names, ['remote.whalerein', 'remote.settings'])
      callback(ctx)
    },
  }
  assert.deepEqual(plugin.inject, ['slots', 'locale', 'configForms', 'remote'])
  await plugin.apply(ctx)
  const entry = registered.find((item) => item?.options?.name === 'settings.section')
  assert.ok(entry, 'settings section registration')
  assert.equal(typeof entry.options.inject, 'function')
  const injected = entry.options.inject()
  assert.ok(injected.status, 'status store injected')
  assert.ok(injected.developerTools, 'developer tools injected')
  const render = () => react.render(entry.Component, {
    t: ctx.locale.bind('settings.whalerein'),
    ...injected,
  })
  const settle = async () => {
    await new Promise((resolve) => setImmediate(resolve))
    await new Promise((resolve) => setImmediate(resolve))
  }
  return {
    writes,
    registered,
    form,
    locales,
    entry,
    render,
    settle,
    remote,
    secrets: injected.secrets,
    getStatus: () => currentStatus,
    setStatus(value) {
      currentStatus = value
    },
    setCloudState(state, extra = {}) {
      currentStatus = { ...currentStatus, state, ...extra }
    },
    calls,
    setActionResult(name, value) { actionResults[name] = value },
    async refresh() {
      await injected.status.refresh()
    },
    mutations,
    setMutateResult(value) { mutateResult = value },
    setFormSnapshot(value) {
      formSnapshot = value
      for (const listener of formListeners) listener()
    },
    setRemoteFailure(value) { currentRemoteFailure = value },
    statusStore: injected.status,
    setDevelopment(value) {
      developerMode = value
      for (const listener of developerListeners) listener()
    },
    revoked,
    setInvite(value) {
      inviteResult = value
    },
    setRevoke(value) {
      revokeResult = value
    },
    async dispose() {
      await react.dispose()
      for (const dispose of disposers) await dispose()
    },
  }
}

test('remote descriptors keep the gateway contract: unique endpoints, no reserved member names', async () => {
  const runtime = await createSettingsRuntime()
  try {
    const contribution = runtime.registered.find((item) => item.contribution)?.contribution
    assert.equal(contribution.package, 'whalerein-dsh')
    // DSH's RemoteNamespaceService fields and prototype members. A descriptor
    // method colliding with one of them fails the whole mount with
    // "client api: method ... conflicts with its namespace service", and the
    // entry then surfaces as "failed" during web boot. A mount stub cannot
    // enforce this, so the names are pinned against the gateway contract
    // (verified with @deepseek-ai/dsh-api-gateway 0.2.2-rc.1).
    const reserved = new Set([
      'ctx', 'empty', 'invokeRemote', 'methods', 'name', 'namespace',
      'assertMethodAvailable', 'has', 'install', 'installDirect', 'installScoped', 'remove',
    ])
    assert.deepEqual(contribution.descriptors.map((descriptor) => descriptor.method), [
      'snapshot', 'invite', 'revoke', 'testConnection',
      'details', 'startConnect', 'cancel', 'retry', 'disconnect', 'reconnect',
      'removeComputer', 'switchProfile', 'enableStaging', 'addCustomService', 'deleteProfile',
    ])
    const endpoints = contribution.descriptors.map((descriptor) => `${descriptor.namespace}/${descriptor.method}`)
    assert.equal(new Set(endpoints).size, endpoints.length, 'remote endpoints must be unique')
    for (const descriptor of contribution.descriptors) {
      assert.ok(!reserved.has(descriptor.method), `${descriptor.id} uses reserved method ${descriptor.method}`)
      assert.equal(descriptor.id, `whalerein-dsh#${descriptor.namespace}/${descriptor.method}`)
      assert.equal(descriptor.namespace, 'whalerein')
      assert.equal(descriptor.service, 'whalereinSettings')
    }
  } finally {
    await runtime.dispose()
  }
})

test('when connected, the settings page shows the host pairing code and paired devices', async () => {
  const runtime = await createSettingsRuntime()
  try {
    assert.equal(loaded.id, 'whalerein-dsh')
    assert.equal(runtime.locales.namespace, 'settings.whalerein')
    assert.equal(runtime.locales.dictionaries.en.nav, 'WhaleRein')
    assert.equal(runtime.entry.options.id, 'whalerein')
    assert.equal(runtime.entry.options.order, 35)
    assert.equal(runtime.entry.options.label(), 'WhaleRein')
    const contribution = runtime.registered.find((item) => item.contribution)?.contribution
    assert.equal(contribution.package, 'whalerein-dsh')
    assert.equal(contribution.descriptors[0].id, 'whalerein-dsh#whalerein/snapshot')
    assert.equal(contribution.descriptors[1].id, 'whalerein-dsh#whalerein/invite')
    assert.equal(contribution.descriptors[2].id, 'whalerein-dsh#whalerein/revoke')
    assert.equal(contribution.descriptors[2].method, 'revoke')
    assert.equal(contribution.descriptors[2].parameters[0].wire, 'deviceId')
    assert.equal(contribution.descriptors[2].parameters[0].source, 'json')
    assert.equal(contribution.descriptors[0].service, 'whalereinSettings')
    assert.equal(contribution.descriptors[1].method, 'invite')

    runtime.render()
    await runtime.settle()
    runtime.render()
    const tree = runtime.render()
    const rendered = text(tree)
    assert.match(rendered, /已连接服务器/)
    assert.doesNotMatch(rendered, /安全地远程控制|交给客户端完成配对|以下设备已获得连接当前主机的授权|保存在当前 profile|端口留空即使用默认值/)
    assert.match(rendered, /生成配对链接/)
    assert.match(rendered, /主机指纹/)
    assert.match(rendered, /abcd ef01 2345 6789/)
    assert.doesNotMatch(rendered, /whalerein:\/\/pair\?/)
    assert.match(rendered, /已配对设备/)
    assert.match(rendered, /手机/)
    assert.match(rendered, /device-phone/)
    assert.match(rendered, /device-laptop/)
    assert.match(rendered, /撤销配对/)
    const phoneRow = find(tree, (node) => node.type === 'li' && text(node).includes('手机'))
    const laptopRow = find(tree, (node) => node.type === 'li' && text(node).includes('device-laptop'))
    assert.equal(find(phoneRow, (node) => node.type === UI.IconLinkOutlineRegular).type, UI.IconLinkOutlineRegular)
    assert.match(text(phoneRow), /iOS · device-phone/)
    const phoneStatus = find(phoneRow, (node) => node.type === UI.Tag && text(node) === '已连接')
    const laptopStatus = find(laptopRow, (node) => node.type === UI.Tag && text(node) === '已配对')
    assert.equal(phoneStatus.props.tone, 'success')
    assert.equal(laptopStatus.props.tone, 'quiet')
    assert.equal(find(phoneStatus, (node) => node.type === UI.StateDot).props.state, 'done')
    assert.equal(find(laptopStatus, (node) => node.type === UI.StateDot).props.state, 'idle')
    assert.equal(find(phoneRow, (node) => node.type === UI.Button && text(node) === '撤销配对').props.variant, 'ghost')
    assert.equal(find(laptopRow, (node) => node.type === UI.IconLinkOutlineRegular).type, UI.IconLinkOutlineRegular)
    assert.doesNotMatch(text(laptopRow), /iOS|macOS/)
    assert.match(rendered, /2 台设备/)
    const configuration = find(tree, (node) => node.type === UI.DisclosureRow && node.props.title === '连接配置')
    assert.ok(configuration)
    assert.equal(configuration.props.open, false)
    assert.match(text(configuration), /服务器/)
    assert.doesNotMatch(text(configuration), /管理员 Token/)
    assert.doesNotMatch(text(configuration), /留空都表示保持当前值|已保存的 Token 会显示为圆点|生成配对链接前请先保存/)
    const hostToken = find(tree, (node) => node.type === UI.Input && node.props.type === 'password' && node.props['aria-label'] === '主机 Token')
    assert.equal(hostToken?.props.type, 'password')
    assert.equal(hostToken?.props.value, '')
    assert.doesNotMatch(rendered, /hidden/)
    await runtime.secrets.refresh()
    assert.match(text(configuration), /主机 ID/)
    assert.match(text(configuration), /主机 Token/)
    assert.match(text(configuration), /端口留空时，加密用 443 和 8883/)
    const advanced = find(configuration, (node) => node.type === UI.DisclosureRow && node.props.title === '高级配置')
    assert.ok(advanced)
    assert.match(text(advanced), /服务端口/)
    assert.match(text(advanced), /传输端口/)
    assert.match(text(advanced), /CA 证书路径/)
    assert.match(text(advanced), /日志级别/)
    assert.doesNotMatch(text(advanced), /允许明文|状态目录|适配器|管理员 Token/)
    assert.equal(find(tree, (node) => node.type === 'textarea'), undefined)
    assert.doesNotMatch(rendered, /运行诊断|Noise XX|pnpm start|调试/)

    const generate = find(tree, (node) => node.type === UI.Button && text(node) === '生成配对链接')
    assert.ok(generate)
    await generate.props.onClick()
    const generated = runtime.render()
    const uri = 'whalerein://pair?v=1&host=host-real&token=once'
    assert.match(text(generated), /配对链接/)
    assert.match(text(generated), /whalerein:\/\/pair\?v=1&host=host-real&token=once/)
    const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator')
    const copied = []
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText: async (value) => copied.push(value) } },
    })
    try {
      const copy = find(generated, (node) => node.type === UI.Button && text(node) === '复制配对链接')
      assert.ok(copy)
      await copy.props.onClick()
      assert.deepEqual(copied, [uri])
      assert.match(text(runtime.render()), /已复制配对链接/)
    } finally {
      if (originalNavigator) Object.defineProperty(globalThis, 'navigator', originalNavigator)
      else delete globalThis.navigator
    }
  } finally {
    await runtime.dispose()
  }
})

test('when a device is selected for revocation, the settings page asks to confirm its ID first', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    const ask = find(runtime.render(), (node) => node.type === UI.Button && text(node) === '撤销配对')
    assert.equal(ask?.props.variant, 'ghost')
    assert.equal(ask.props.style.color, 'var(--dsw-alias-state-error-primary)')
    await ask.props.onClick()
    const confirming = runtime.render()
    assert.match(text(confirming), /确认撤销/)
    assert.match(text(confirming), /device-phone/)
    const confirm = find(confirming, (node) => node.type === UI.Button && text(node) === '确认撤销')
    const cancel = find(confirming, (node) => node.type === UI.Button && text(node) === '取消')
    assert.equal(confirm.props.variant, 'primary')
    assert.equal(confirm.props.style['--dsw-alias-button-primary-fill'], 'var(--dsw-alias-state-error-primary)')
    assert.equal(cancel.props.variant, 'ghost')
    assert.equal(cancel.props.className, undefined)
    await cancel.props.onClick()
    assert.equal(find(runtime.render(), (node) => node.type === UI.Button && text(node) === '确认撤销'), undefined)

    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '撤销配对').props.onClick()
    runtime.setStatus({ ...runtime.getStatus(), devices: [] })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '确认撤销').props.onClick()
    assert.deepEqual(runtime.revoked, ['device-phone'])
    await runtime.settle()
    assert.doesNotMatch(text(runtime.render()), /手机|device-phone/)

    runtime.setStatus({
      ...runtime.getStatus(),
      devices: [{ deviceId: 'device-phone', displayName: '手机', fingerprint: 'f1', pairedAt: 1700000000000, connected: true }],
    })
    await runtime.refresh()
    runtime.setRevoke({ ok: false, error: { code: 'whalerein/unauthorized' } })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '撤销配对').props.onClick()
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '确认撤销').props.onClick()
    const failed = text(runtime.render())
    assert.match(failed, /服务器拒绝了这台主机。/)
    assert.match(failed, /手机/)
    assert.match(failed, /device-phone/)
    assert.deepEqual(runtime.revoked, ['device-phone', 'device-phone'])
  } finally {
    await runtime.dispose()
  }
})

test('device rows reuse the official pairing icon without guessing hardware from names', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    const rows = findAll(runtime.render(), (node) => node.type === 'li')
    assert.equal(rows.length, 2)
    for (const row of rows) assert.ok(find(row, (node) => node.type === UI.IconLinkOutlineRegular))
  } finally {
    await runtime.dispose()
  }
})

test('official disclosures toggle independently and the log menu updates the saved draft', async () => {
  const runtime = await createSettingsRuntime({ development: true })
  try {
    runtime.render()
    await runtime.settle()
    runtime.render()
    const disclosure = (title) => find(runtime.render(), (node) => node.type === UI.DisclosureRow && node.props.title === title)
    assert.equal(disclosure('连接配置').props.open, false)
    disclosure('连接配置').props.onToggle()
    assert.equal(disclosure('连接配置').props.open, true)
    disclosure('高级配置').props.onToggle()
    assert.equal(disclosure('高级配置').props.open, true)
    assert.equal(disclosure('连接配置').props.open, true)
    disclosure('运行诊断').props.onToggle()
    assert.equal(disclosure('运行诊断').props.open, true)
    assert.equal(disclosure('连接配置').props.open, true)
    assert.equal(disclosure('高级配置').props.open, true)

    const menu = () => find(runtime.render(), (node) => node.type === UI.Menu)
    assert.equal(menu().props.open, false)
    assert.equal(menu().props.anchor.props.size, 'md')
    assert.equal(menu().props.anchor.props.variant, 'ghost')
    assert.equal(menu().props.align, 'end')
    menu().props.anchor.props.onClick()
    assert.equal(menu().props.open, true)
    assert.equal(menu().props.selectedId, 'debug')
    disclosure('高级配置').props.onToggle()
    assert.equal(menu().props.open, false)
    disclosure('高级配置').props.onToggle()
    assert.equal(menu().props.open, false)
    menu().props.anchor.props.onClick()
    menu().props.onSelect('warn')
    assert.equal(menu().props.open, false)
    assert.equal(menu().props.selectedId, 'warn')
    const settings = find(runtime.render(), (node) => node.type === UI.Button && text(node) === '保存修改')
    assert.equal(settings.props.disabled, false)
    await settings.props.onClick()
    assert.ok(runtime.writes.some(([op, key, value]) => op === 'set' && key === 'log_level' && value === 'warn'))
  } finally {
    await runtime.dispose()
  }
})

test('a read-only profile disables official fields, menu and form without writing settings', async () => {
  const runtime = await createSettingsRuntime({ writable: false })
  try {
    runtime.render()
    await runtime.settle()
    const tree = runtime.render()
    const fields = findAll(tree, (node) => node.type === UI.Input)
    assert.equal(fields.length, 6)
    assert.ok(fields.every((field) => field.props.disabled === true))
    assert.equal(new Set(fields.map((field) => field.props.id)).size, fields.length)
    const menu = find(tree, (node) => node.type === UI.Menu)
    assert.equal(menu.props.anchor.props.disabled, true)
    assert.equal(menu.props.open, false)
    const settings = find(tree, (node) => node.type === UI.Button && text(node) === '保存修改')
    assert.equal(settings.props.disabled, true)
    await settings.props.onClick()
    assert.deepEqual(runtime.writes, [])
  } finally {
    await runtime.dispose()
  }
})

test('when the host ID field is cleared, saving preserves the stored ID', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    const input = find(runtime.render(), (node) => node.type === UI.Input && node.props['aria-label'] === '主机 ID')
    assert.equal(input.props.value, 'host-real')
    input.props.onChange({ target: { value: '   ' } })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '保存修改').props.onClick()
    assert.equal(runtime.writes.some(([, key]) => key === 'host_id'), false)
    const renamed = find(runtime.render(), (node) => node.type === UI.Input && node.props['aria-label'] === '主机 ID')
    renamed.props.onChange({ target: { value: 'office-host' } })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '保存修改').props.onClick()
    assert.ok(runtime.writes.some(([op, key, value]) => op === 'set' && key === 'host_id' && value === 'office-host'))
  } finally {
    await runtime.dispose()
  }
})

test('when disconnected, settings opens connection controls and hides debug details by default', async () => {
  const runtime = await createSettingsRuntime({ connected: false })
  try {
    runtime.render()
    await runtime.settle()
    const tree = runtime.render()
    assert.match(text(tree), /未连接服务器/)
    assert.match(text(tree), /主机会自动重新连接/)
    assert.doesNotMatch(text(tree), /请检查下方服务器配置/)
    assert.match(text(tree), /生成配对链接/)
    assert.equal(find(tree, (node) => node.type === UI.DisclosureRow).props.open, true)
    assert.equal(find(tree, (node) => node.type === 'code' && text(node).includes('abcd')), undefined)
    const save = find(tree, (node) => node.type === UI.Button && text(node) === '保存修改')
    await save.props.onClick()
    assert.equal(runtime.writes.some(([, key]) => key === 'mqtt_url' || key === 'access_url'), false)
    assert.equal(runtime.writes.some(([, key]) => key === 'admin_token' || key === 'host_token'), false)
    assert.match(text(tree), /device-phone/)
    assert.doesNotMatch(text(tree), /已覆盖|继承默认/)
    assert.doesNotMatch(text(tree), /运行诊断|Noise XX|pnpm start/)
  } finally {
    await runtime.dispose()
  }
})

test('when developer mode is enabled, diagnostics appear and configuration stays editable', async () => {
  const runtime = await createSettingsRuntime({ development: true })
  try {
    runtime.render()
    await runtime.settle()
    const tree = runtime.render()
    assert.match(text(tree), /运行诊断/)
    assert.doesNotMatch(text(tree), /开发命令|pnpm start|pnpm dev|HTTP 监听|Noise XX/)
    const detail = (label, root = tree) => text(find(root, (node) => node.type === 'div'
      && node.children?.[0]?.type === 'dt' && text(node.children[0]) === label)?.children[1])
    assert.equal(detail('运行中的主机 ID'), 'host-real')
    assert.equal(detail('DSH 可用性'), '就绪')
    assert.equal(detail('撤销记录同步'), '等待同步')
    assert.equal(detail('MQTT 重连次数'), '2')
    assert.equal(detail('MQTT 发布失败数'), '1')
    assert.equal(detail('待发送密文数'), '3')
    assert.equal(detail('结果未知的命令数'), '4')
    assert.equal(detail('需要重新同步的会话数'), '5')
    const input = find(tree, (node) => node.type === UI.Input)
    assert.equal(input?.props['aria-label'], '服务器')
    input.props.onChange({ target: { value: 'access.example' } })
    const button = find(runtime.render(), (node) => node.type === UI.Button && text(node) === '保存修改')
    assert.ok(button)
    await button.props.onClick()
    assert.ok(runtime.writes.some(([op, key, value]) => op === 'set' && key === 'server' && value === 'access.example'))
    assert.ok(runtime.writes.some(([op, key]) => op === 'unset' && key === 'mqtt_url'))
    assert.ok(runtime.writes.some(([op, key]) => op === 'unset' && key === 'access_url'))
    assert.equal(runtime.writes.some(([op, key]) => op === 'unset' && ['access_port', 'mqtt_port', 'ca_file'].includes(key)), false)
    assert.equal(runtime.writes.some(([, key]) => ['adapter', 'state_dir', 'allow_insecure_mqtt', 'admin_token'].includes(key)), false)
    assert.ok(runtime.writes.every(([, key, value]) => key !== 'host_token' && key !== 'admin_token' && value !== SECRET_MASK))

    runtime.setStatus({ ...runtime.getStatus(), diagnostics: null })
    await runtime.refresh()
    assert.equal(detail('MQTT 重连次数', runtime.render()), '—')
    assert.equal(detail('撤销记录同步', runtime.render()), '—')

    runtime.setDevelopment(false)
    assert.doesNotMatch(text(runtime.render()), /运行诊断|Noise XX|pnpm start/)
  } finally {
    await runtime.dispose()
  }
})

test('the official password input shows presence and stores only a typed replacement', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    runtime.render()
    await runtime.settle()
    const host = find(runtime.render(), (node) => node.type === UI.Input && node.props.type === 'password')
    assert.match(text(runtime.render()), /已配置/)
    assert.equal(host.props.value, '')
    assert.equal(host.props.autoComplete, 'new-password')
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '保存修改').props.onClick()
    assert.equal(runtime.writes.some(([, key]) => key === 'host_token'), false)
    host.props.onChange({ target: { value: 'replacement-token' } })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '保存修改').props.onClick()
    assert.ok(runtime.writes.some(([op, key, value]) => op === 'set' && key === 'host_token' && value === 'replacement-token'))
    assert.equal(runtime.writes.some(([, key]) => key === 'admin_token'), false)
  } finally {
    await runtime.dispose()
  }
})

test('each settings control has a title and an associated subtitle', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    const tree = runtime.render()
    const inputs = findAll(tree, (node) => node.type === UI.Input)
    assert.equal(inputs.length, 6)
    for (const input of inputs) {
      const label = find(tree, (node) => node.type === 'label' && node.props.htmlFor === input.props.id)
      assert.equal(text(label), input.props['aria-label'])
      const subtitle = find(tree, (node) => node.props?.id === input.props['aria-describedby'])
      assert.ok(text(subtitle).length > 10, input.props['aria-label'])
    }
    const menu = find(tree, (node) => node.type === UI.Menu)
    const subtitle = find(tree, (node) => node.props?.id === menu.props.anchor.props['aria-describedby'])
    assert.match(text(subtitle), /诊断信息/)
  } finally {
    await runtime.dispose()
  }
})

test('testing a draft does not save and discards results after edits', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    const button = (name) => find(runtime.render(), (node) => node.type === UI.Button && text(node) === name)
    const field = () => find(runtime.render(), (node) => node.type === UI.Input && node.props['aria-label'] === '服务器')
    let finish
    let tested
    runtime.remote.whalerein.testConnection = (draft) => {
      tested = draft
      return new Promise((resolve) => { finish = resolve })
    }
    field().props.onChange({ target: { value: 'draft.example' } })
    const pending = button('测试连接').props.onClick()
    assert.equal(tested.server, 'draft.example')
    assert.equal(tested.host_token, '')
    assert.equal(button('测试中…').props.disabled, true)
    assert.equal(button('保存修改').props.disabled, true)
    field().props.onChange({ target: { value: 'new.example' } })
    finish({ ok: true, value: { configuration: { ok: true }, access: { ok: true }, mqtt: { ok: true, authenticated: true }, running: 'connected' } })
    await pending
    assert.doesNotMatch(text(runtime.render()), /连接测试通过/)
    assert.deepEqual(runtime.writes, [])

    runtime.remote.whalerein.testConnection = async () => ({ ok: true, value: {
      configuration: { ok: true }, access: { ok: true }, mqtt: { ok: true, authenticated: true }, running: 'not_checked',
    } })
    await button('测试连接').props.onClick()
    const success = find(runtime.render(), (node) => node.type === 'p' && node.props.role === 'status')
    assert.equal(text(success), '连接测试通过。')

    runtime.remote.whalerein.testConnection = async () => ({ ok: true, value: {
      configuration: { ok: true }, access: { ok: true }, mqtt: { ok: false, code: 'tls' }, running: 'not_checked',
    } })
    await button('测试连接').props.onClick()
    assert.match(text(runtime.render()), /证书或 TLS 验证失败/)
    const failure = find(runtime.render(), (node) => node.type === 'p' && node.props.role === 'status')
    assert.equal(text(failure), '草稿检查失败（登录验证）：证书或 TLS 验证失败')
    field().props.onChange({ target: { value: 'changed.example' } })
    assert.doesNotMatch(text(runtime.render()), /证书或 TLS 验证失败/)
    runtime.remote.whalerein.testConnection = async () => { throw new Error('private failure') }
    await button('测试连接').props.onClick()
    assert.match(text(runtime.render()), /测试接口调用失败，请重新加载插件后重试。/)
    assert.doesNotMatch(text(runtime.render()), /配置检查失败|网络连接失败/)
    assert.doesNotMatch(text(runtime.render()), /private failure/)
    runtime.remote.whalerein.testConnection = async () => ({ ok: false, error: { code: 'gateway/internal', message: 'private failure' } })
    await button('测试连接').props.onClick()
    assert.match(text(runtime.render()), /测试接口调用失败，请重新加载插件后重试。/)
    runtime.remote.whalerein.testConnection = async () => ({ ok: false, error: { code: 'gateway/method-unavailable' } })
    await button('测试连接').props.onClick()
    assert.match(text(runtime.render()), /测试接口尚未就绪，请重新加载插件后重试。/)
    runtime.remote.whalerein.testConnection = async () => ({ ok: true, value: {
      configuration: { ok: true }, access: { ok: true }, mqtt: { ok: false, code: 'credentials' },
    } })
    await button('测试连接').props.onClick()
    assert.match(text(runtime.render()), /登录被拒绝，请检查主机 Token 和服务端版本。/)
    assert.doesNotMatch(text(runtime.render()), /MQTT/)
    runtime.remote.whalerein.testConnection = async () => ({ ok: true, value: { configuration: { ok: false, code: 'rate_limited' } } })
    await button('测试连接').props.onClick()
    assert.match(text(runtime.render()), /测试过于频繁，请稍后重试。/)
    runtime.remote.whalerein.testConnection = async () => ({ ok: true, value: {
      configuration: { ok: true }, access: { ok: true }, mqtt: { ok: true },
    } })
    await button('测试连接').props.onClick()
    assert.match(text(runtime.render()), /测试接口尚未就绪/)
    assert.doesNotMatch(text(runtime.render()), /连接测试通过/)
  } finally {
    await runtime.dispose()
  }
})

test('Access readiness failure does not contradict an established MQTT connection', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    runtime.remote.whalerein.testConnection = async () => ({ ok: true, value: {
      configuration: { ok: true }, access: { ok: false, code: 'not_ready' }, mqtt: { ok: true, authenticated: true }, running: 'connected',
    } })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '测试连接').props.onClick()
    const tree = runtime.render()
    assert.match(text(tree), /当前主机已连接服务器/)
    const result = find(tree, (node) => node.type === 'p' && node.props.role === 'status')
    assert.equal(text(result), 'Access 服务检查失败：Access 未返回就绪状态 200；现有主机连接仍在线。')
    runtime.setStatus({ ...runtime.getStatus(), connected: false })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '测试连接').props.onClick()
    const refreshed = text(runtime.render())
    assert.match(refreshed, /当前主机未连接服务器/)
    assert.doesNotMatch(refreshed, /仍在线/)
  } finally {
    await runtime.dispose()
  }
})

test('when a saved secret is absent or unreadable, token inputs remain empty', async () => {
  const cases = [
    async () => ({ ok: false, error: { message: 'unavailable' } }),
    async () => { throw new Error('settings unavailable') },
    async () => ({ ok: true, value: { namespaces: [{ ns: 'whalerein-dsh' }] } }),
    async () => ({ ok: true, value: { namespaces: [{ ns: 'whalerein-dsh', secrets: [{ path: ['admin_token'], set: false }] }] } }),
  ]
  for (const describe of cases) {
    const runtime = await createSettingsRuntime({ describe })
    try {
      runtime.render()
      await runtime.settle()
      const tree = runtime.render()
      const host = find(tree, (node) => node.type === UI.Input && node.props.type === 'password' && node.props['aria-label'] === '主机 Token')
      assert.equal(find(tree, (node) => node.type === UI.Input && node.props.type === 'password' && node.props['aria-label'] === '管理员 Token'), undefined)
      assert.equal(host.props.value, '')
      assert.match(text(tree), /未配置/)
      assert.equal(find(runtime.render(), (node) => node.type === UI.Input && node.props.type === 'password' && node.props['aria-label'] === '主机 Token').props.value, '')
    } finally {
      await runtime.dispose()
    }
  }
})

test('when settings status is requested, it reads the snapshot through the injected remote namespace', async () => {
  const calls = []
  const registered = []
  const currentStatus = {
    available: true,
    connected: true,
    hostId: 'host-real',
    pairingCode: 'abcd ef01',
    devices: [],
  }
  class RemoteService extends Service {
    constructor(ctx) {
      super(ctx, 'remote')
      const owner = ctx
      this.$mount = async (contribution) => {
        registered.push({ contribution })
        await owner.plugin({
          name: 'remote-namespace',
          apply(child) {
            const service = new Service(child, 'remote.whalerein')
            service.snapshot = async () => {
              calls.push('snapshot')
              return { ok: true, value: currentStatus }
            }
            const settings = new Service(child, 'remote.settings')
            settings.describe = async () => ({ ok: true, value: { namespaces: [] } })
          },
        })
        return async () => {}
      }
    }
  }
  class LocaleService extends Service {
    constructor(ctx) {
      super(ctx, 'locale')
    }

    bind() {
      return (key) => key
    }

    register() {
      return () => {}
    }
  }
  class ConfigFormsService extends Service {
    constructor(ctx) {
      super(ctx, 'configForms')
      this.developerTools = {
        enabled: {
          getSnapshot: () => false,
          subscribe: () => () => {},
        },
      }
    }

    get(entry) {
      assert.equal(entry, 'whalerein-dsh')
      return {
        getSnapshot: () => ({ status: 'ready', value: {}, user: {}, writable: true, revision: 1 }),
        subscribe: () => () => {},
      }
    }
  }
  class SlotsService extends Service {
    constructor(ctx) {
      super(ctx, 'slots')
      this.registered = registered
    }

    inject(_name, register) {
      registered.push(register())
      return () => {}
    }

    register(options, Component) {
      const entry = { options, Component }
      registered.push(entry)
      return () => {}
    }
  }

  const root = new Context()
  try {
    await root.plugin({
      name: 'fixtures',
      apply(ctx) {
        new RemoteService(ctx)
        new LocaleService(ctx)
        new ConfigFormsService(ctx)
        new SlotsService(ctx)
      },
    })
    const react = createReactRuntime()
    const plugin = loaded.factory(requireClient(react.React))
    await root.plugin(plugin)
    const entry = registered.find((item) => item?.options?.name === 'settings.section')
    assert.ok(entry, 'settings section registration')
    const status = entry.options.inject().status
    await status.refresh()
    assert.deepEqual(calls, ['snapshot'])
    assert.equal(status.getSnapshot().available, true)
    assert.equal(status.getSnapshot().connected, true)
    assert.equal(status.getSnapshot().pairingCode, 'abcd ef01')
  } finally {
    await root.fiber.dispose()
  }
})

test('when Access rejects an invitation, settings explains the error and hides the link', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.setInvite({ ok: false, error: { code: 'whalerein/unauthorized' } })
    runtime.render()
    await runtime.settle()
    const generate = find(runtime.render(), (node) => node.type === UI.Button && text(node) === '生成配对链接')
    await generate.props.onClick()
    const rendered = text(runtime.render())
    assert.match(rendered, /服务器拒绝了这台主机/)
    assert.doesNotMatch(rendered, /whalerein:\/\/pair\?/)
    runtime.setInvite(Object.assign(new Error('boom'), { code: 'whalerein/server-missing' }))
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '生成配对链接').props.onClick()
    assert.match(text(runtime.render()), /请先保存服务器地址/)
  } finally {
    await runtime.dispose()
  }
})

test('when remote status loading fails, settings keeps configuration open and offers retry', async () => {
  const runtime = await createSettingsRuntime({ remoteFailure: true })
  try {
    runtime.render()
    await runtime.settle()
    const tree = runtime.render()
    assert.match(text(tree), /暂时无法获取连接状态/)
    assert.equal(find(tree, (node) => node.type === UI.DisclosureRow).props.open, true)
    const retry = find(tree, (node) => node.type === UI.Button && text(node) === '刷新')
    assert.ok(retry)
    await retry.props.onClick()
  } finally {
    await runtime.dispose()
  }
})

const saveButton = (runtime) => find(runtime.render(), (node) => node.type === UI.Button && text(node) === '保存修改')
const field = (runtime, label) => find(runtime.render(), (node) => node.type === UI.Input && node.props['aria-label'] === label)
const deviceRow = (runtime, id) => find(runtime.render(), (node) => node.type === 'li' && text(node).includes(id))

test('clearing the server without a legacy MQTT URL is blocked per field instead of saving an unstartable profile', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.setFormSnapshot({
      status: 'ready',
      value: { server: 'access.example', mqtt_url: 'mqtts://access.example:8883', access_url: 'https://access.example', host_id: 'host-real', host_token: 'hidden', log_level: 'info' },
      user: { server: 'access.example' }, base: {}, writable: true, revision: 4,
    })
    runtime.render()
    await runtime.settle()
    const server = field(runtime, '服务器')
    server.props.onChange({ target: { value: '' } })
    await saveButton(runtime).props.onClick()
    assert.deepEqual(runtime.mutations, [])
    assert.deepEqual(runtime.writes, [])
    const tree = runtime.render()
    assert.match(text(tree), /请输入服务器地址/)
    const invalid = find(tree, (node) => node.props?.['aria-describedby']?.includes('server-error') && node.type === UI.Input)
    assert.ok(invalid)
  } finally {
    await runtime.dispose()
  }
})

test('saving writes every changed field in one atomic mutation at the revision the draft was read from', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    field(runtime, '服务器').props.onChange({ target: { value: 'access.example' } })
    field(runtime, '服务端口').props.onChange({ target: { value: '8443' } })
    await saveButton(runtime).props.onClick()
    assert.equal(runtime.mutations.length, 1)
    assert.equal(runtime.mutations[0].revision, 3)
    const ops = runtime.mutations[0].ops
    assert.ok(ops.some((op) => op.op === 'set' && op.path[0] === 'server' && op.value === 'access.example'))
    assert.ok(ops.some((op) => op.op === 'set' && op.path[0] === 'access_port' && op.value === '8443'))
    assert.ok(ops.some((op) => op.op === 'unset' && op.path[0] === 'mqtt_url'))
  } finally {
    await runtime.dispose()
  }
})

test('a rejected atomic save keeps the draft and distinguishes a concurrent revision change', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    field(runtime, '服务器').props.onChange({ target: { value: 'draft.example' } })
    runtime.form.mutate = async () => { runtime.setFormSnapshot({ ...runtime.form.getSnapshot(), revision: 9 }); return false }
    await saveButton(runtime).props.onClick()
    assert.match(text(runtime.render()), /设置已在另一个窗口修改/)
    assert.equal(field(runtime, '服务器').props.value, 'draft.example')
  } finally {
    await runtime.dispose()
  }
})

test('an external revision keeps a dirty draft, offers the saved values, and guards the unload', async () => {
  const runtime = await createSettingsRuntime()
  const listeners = new Map()
  const originalWindow = globalThis.window
  try {
    globalThis.window = { ...originalWindow,
      addEventListener: (name, handler) => listeners.set(name, handler),
      removeEventListener: (name) => listeners.delete(name) }
    runtime.render()
    await runtime.settle()
    field(runtime, '服务器').props.onChange({ target: { value: 'typed.example' } })
    runtime.render()
    assert.ok(listeners.has('beforeunload'))
    let prevented = false
    listeners.get('beforeunload')({ preventDefault() { prevented = true }, returnValue: '' })
    assert.equal(prevented, true)

    runtime.setFormSnapshot({
      status: 'ready',
      value: { server: 'saved.example', mqtt_url: 'mqtts://saved.example:8883', host_id: 'host-real', host_token: 'hidden', log_level: 'debug' },
      user: { server: 'saved.example' }, base: {}, writable: true, revision: 5,
    })
    runtime.render()
    await runtime.settle()
    runtime.render()
    assert.equal(field(runtime, '服务器').props.value, 'typed.example')
    assert.match(text(runtime.render()), /设置已在另一个窗口修改/)
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '载入已保存的值').props.onClick()
    assert.equal(field(runtime, '服务器').props.value, 'saved.example')
    assert.doesNotMatch(text(runtime.render()), /设置已在另一个窗口修改/)
    assert.equal(listeners.has('beforeunload'), false)
  } finally {
    globalThis.window = originalWindow
    await runtime.dispose()
  }
})

test('changing the host ID requires explicit confirmation because it creates a new host identity', async () => {
  const runtime = await createSettingsRuntime()
  const originalWindow = globalThis.window
  try {
    globalThis.window = { ...originalWindow, confirm: () => false }
    runtime.render()
    await runtime.settle()
    field(runtime, '主机 ID').props.onChange({ target: { value: 'renamed-host' } })
    await saveButton(runtime).props.onClick()
    assert.deepEqual(runtime.writes, [])
    globalThis.window = { ...originalWindow, confirm: () => true }
    await saveButton(runtime).props.onClick()
    assert.ok(runtime.writes.some(([op, key, value]) => op === 'set' && key === 'host_id' && value === 'renamed-host'))
  } finally {
    globalThis.window = originalWindow
    await runtime.dispose()
  }
})

test('an expired pairing link keeps its expiry and disables copying with a clear reason', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.setInvite({ ok: true, value: { pairingUri: 'whalerein://pair?v=1&host=host-real&token=once', expiresAtUnixMs: Date.now() - 1000, fingerprint: 'f' } })
    runtime.render()
    await runtime.settle()
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '生成配对链接').props.onClick()
    const tree = runtime.render()
    assert.match(text(tree), /该配对链接已过期/)
    assert.doesNotMatch(text(tree), /已复制配对链接/)
    const copy = find(tree, (node) => node.type === UI.Button && text(node) === '复制配对链接')
    assert.equal(copy.props.disabled, true)
    const copied = []
    const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator')
    Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { clipboard: { writeText: async (value) => copied.push(value) } } })
    try {
      await copy.props.onClick()
      assert.deepEqual(copied, [])
    } finally {
      if (originalNavigator) Object.defineProperty(globalThis, 'navigator', originalNavigator)
      else delete globalThis.navigator
    }
  } finally {
    await runtime.dispose()
  }
})

test('one revoke runs at a time and a second row cannot retarget the in-flight confirmation', async () => {
  const runtime = await createSettingsRuntime()
  try {
    let finish
    runtime.remote.whalerein.revoke = (deviceId) => { runtime.revoked.push(deviceId); return new Promise((resolve) => { finish = resolve }) }
    runtime.render()
    await runtime.settle()
    const phone = deviceRow(runtime, 'device-phone')
    await find(phone, (node) => node.type === UI.Button && text(node) === '撤销配对').props.onClick()
    const confirmButton = find(deviceRow(runtime, 'device-phone'), (node) => node.type === UI.Button && text(node) === '确认撤销')
    const pending = confirmButton.props.onClick()
    const laptopAsk = find(deviceRow(runtime, 'device-laptop'), (node) => node.type === UI.Button && text(node) === '撤销配对')
    assert.equal(laptopAsk.props.disabled, true)
    await laptopAsk.props.onClick()
    assert.match(text(deviceRow(runtime, 'device-phone')), /正在撤销/)
    await confirmButton.props.onClick()
    assert.deepEqual(runtime.revoked, ['device-phone'])
    finish({ ok: true, value: { deviceId: 'device-phone' } })
    await pending
    await runtime.settle()
  } finally {
    await runtime.dispose()
  }
})

test('a double click on generate pairing link requests only one one-time invitation', async () => {
  const runtime = await createSettingsRuntime()
  try {
    let calls = 0
    let finish
    runtime.remote.whalerein.invite = () => { calls += 1; return new Promise((resolve) => { finish = resolve }) }
    runtime.render()
    await runtime.settle()
    const generate = find(runtime.render(), (node) => node.type === UI.Button && text(node) === '生成配对链接')
    const first = generate.props.onClick()
    const second = generate.props.onClick()
    finish({ ok: true, value: { pairingUri: 'whalerein://pair?v=1&host=host-real&token=once', expiresAtUnixMs: Date.now() + 60_000 } })
    await Promise.all([first, second])
    await runtime.settle()
    assert.equal(calls, 1)
  } finally {
    await runtime.dispose()
  }
})

test('one failed status poll keeps the last known device list instead of showing an empty host', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    runtime.render()
    assert.match(text(runtime.render()), /device-phone/)
    runtime.setRemoteFailure(true)
    await runtime.refresh()
    const tree = runtime.render()
    assert.match(text(tree), /暂时无法获取连接状态/)
    assert.match(text(tree), /device-phone/)
    assert.match(text(tree), /device-laptop/)
    runtime.setRemoteFailure(false)
    await runtime.refresh()
    assert.match(text(runtime.render()), /device-phone/)
  } finally {
    await runtime.dispose()
  }
})

test('revocation confirmation is keyboard reachable and Escape cancels it with focus returned', async () => {
  const runtime = await createSettingsRuntime()
  const focused = []
  const originalDocument = globalThis.document
  try {
    globalThis.document = { getElementById: (id) => ({ focus: () => focused.push(id) }) }
    runtime.render()
    await runtime.settle()
    await find(deviceRow(runtime, 'device-phone'), (node) => node.type === UI.Button && text(node) === '撤销配对').props.onClick()
    const tree = runtime.render()
    const cancel = find(tree, (node) => node.type === UI.Button && text(node) === '取消')
    assert.equal(cancel.props.autoFocus, true)
    const confirm = find(tree, (node) => node.type === UI.Button && text(node) === '确认撤销')
    assert.equal(confirm.props.autoFocus, undefined)
    const actions = find(tree, (node) => typeof node.props?.onKeyDown === 'function')
    assert.ok(actions)
    actions.props.onKeyDown({ key: 'Escape', preventDefault() {} })
    const after = runtime.render()
    assert.equal(find(after, (node) => node.type === UI.Button && text(node) === '确认撤销'), undefined)
    const ask = find(deviceRow(runtime, 'device-phone'), (node) => node.type === UI.Button && text(node) === '撤销配对')
    assert.equal(ask.props.autoFocus, true)
    assert.ok(focused.some((id) => id.endsWith('-revoke-device-phone')), 'focus returned to the revoke trigger')
  } finally {
    globalThis.document = originalDocument
    await runtime.dispose()
  }
})

test('the settings layout declares a narrow single-column layout and uses theme variables only', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    const css = text(find(runtime.render(), (node) => node.type === 'style'))
    assert.match(css, /@media \(max-width: 560px\)/)
    assert.match(css, /whalerein-field-row \{ grid-template-columns: minmax\(0, 1fr\)/)
    assert.match(css, /whalerein-diagnostic-row \{ grid-template-columns: minmax\(0, 1fr\)/)
    assert.match(css, /var\(--dsw-alias-/)
    assert.doesNotMatch(css, /#[0-9a-fA-F]{3,8}\b|\brgba?\(/)
  } finally {
    await runtime.dispose()
  }
})

test('advanced ports and CA bundle save and clear atomically', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.setFormSnapshot({
      status: 'ready',
      value: { server: 'access.example', mqtt_url: 'mqtts://access.example:8883', access_url: 'https://access.example', host_id: 'host-real', host_token: 'hidden', access_port: '8443', mqtt_port: '8884', ca_file: '/etc/ca.pem', log_level: 'info' },
      user: { server: 'access.example', access_port: '8443', mqtt_port: '8884', ca_file: '/etc/ca.pem' }, base: {}, writable: true, revision: 6,
    })
    runtime.render()
    await runtime.settle()
    const ca = find(runtime.render(), (node) => node.type === UI.Input && node.props['aria-label'] === 'CA 证书路径')
    assert.equal(ca.props.value, '/etc/ca.pem')
    ca.props.onChange({ target: { value: '' } })
    await saveButton(runtime).props.onClick()
    assert.ok(runtime.mutations[0].ops.some((op) => op.op === 'unset' && op.path[0] === 'ca_file'))
  } finally {
    await runtime.dispose()
  }
})

test('every network failure class has a short actionable settings message', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    const cases = [
      ['dns', /无法解析服务器地址/],
      ['refused', /连接被拒绝/],
      ['timeout', /连接超时/],
      ['tls', /证书或 TLS 验证失败/],
      ['network', /网络连接失败/],
    ]
    for (const [code, message] of cases) {
      runtime.remote.whalerein.testConnection = async () => ({ ok: true, value: {
        configuration: { ok: true }, access: { ok: true }, mqtt: { ok: false, code }, running: 'not_checked',
      } })
      await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '测试连接').props.onClick()
      const rendered = text(runtime.render())
      assert.match(rendered, message)
      assert.doesNotMatch(rendered, /private|stack/)
    }
  } finally {
    await runtime.dispose()
  }
})

test('an invalid stored configuration explains the field and does not claim zero paired devices', async () => {
  const runtime = await createSettingsRuntime({ connected: false })
  try {
    runtime.setStatus({
      available: false, connected: false, hostId: '', pairingCode: '', devices: [],
      configError: { code: 'invalid_config', field: 'mqtt_url', reason: 'mqtt_url is required' },
    })
    runtime.render()
    await runtime.settle()
    const tree = runtime.render()
    const rendered = text(tree)
    assert.match(rendered, /已保存的配置无效/)
    assert.match(rendered, /mqtt_url/)
    assert.match(rendered, /mqtt_url is required/)
    assert.doesNotMatch(rendered, /暂无已配对设备/)
    assert.equal(find(tree, (node) => node.props?.id === 'whalerein-devices-title'), undefined)
  } finally {
    await runtime.dispose()
  }
})

test('a pairing link without an expiry stays copyable and reports unknown lifetime', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.setInvite({ ok: true, value: { pairingUri: 'whalerein://pair?v=1&host=host-real&token=once', expiresAtUnixMs: 0 } })
    runtime.render()
    await runtime.settle()
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '生成配对链接').props.onClick()
    const tree = runtime.render()
    assert.match(text(tree), /只能使用一次/)
    assert.equal(find(tree, (node) => node.type === UI.Button && text(node) === '复制配对链接').props.disabled, false)
  } finally {
    await runtime.dispose()
  }
})

test('an invitation result that is not a pairing URI is reported without rendering it', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.setInvite({ ok: true, value: { pairingUri: 'https://private.example/token', expiresAtUnixMs: Date.now() + 1000 } })
    runtime.render()
    await runtime.settle()
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '生成配对链接').props.onClick()
    const rendered = text(runtime.render())
    assert.match(rendered, /暂时无法生成配对链接/)
    assert.doesNotMatch(rendered, /private\.example/)
  } finally {
    await runtime.dispose()
  }
})

test('a clipboard rejection keeps the link visible and reports a copy failure', async () => {
  const runtime = await createSettingsRuntime()
  const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator')
  try {
    runtime.render()
    await runtime.settle()
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '生成配对链接').props.onClick()
    Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { clipboard: { writeText: async () => { throw new Error('denied') } } } })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '复制配对链接').props.onClick()
    const rendered = text(runtime.render())
    assert.match(rendered, /无法复制链接/)
    assert.match(rendered, /whalerein:\/\/pair\?/)
  } finally {
    if (originalNavigator) Object.defineProperty(globalThis, 'navigator', originalNavigator)
    else delete globalThis.navigator
    await runtime.dispose()
  }
})

test('a host without atomic mutate support still saves changed fields in order', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    delete runtime.form.mutate
    field(runtime, '服务器').props.onChange({ target: { value: 'access.example' } })
    await saveButton(runtime).props.onClick()
    assert.ok(runtime.writes.some(([op, key, value]) => op === 'set' && key === 'server' && value === 'access.example'))
    assert.ok(runtime.writes.some(([op, key]) => op === 'unset' && key === 'mqtt_url'))
    assert.match(text(runtime.render()), /设置已保存/)
  } finally {
    await runtime.dispose()
  }
})

test('a blocked confirmation dialog never renames the host', async () => {
  const runtime = await createSettingsRuntime()
  const originalWindow = globalThis.window
  try {
    globalThis.window = { ...originalWindow, confirm: () => { throw new Error('blocked') } }
    runtime.render()
    await runtime.settle()
    field(runtime, '主机 ID').props.onChange({ target: { value: 'renamed-host' } })
    await saveButton(runtime).props.onClick()
    assert.deepEqual(runtime.writes, [])
    assert.equal(field(runtime, '主机 ID').props.value, 'renamed-host')
  } finally {
    globalThis.window = originalWindow
    await runtime.dispose()
  }
})

test('a busy or unavailable test service is reported in one sentence without protocol names', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.render()
    await runtime.settle()
    runtime.remote.whalerein.testConnection = async () => ({ ok: true, value: { configuration: { ok: false, code: 'busy' } } })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '测试连接').props.onClick()
    assert.match(text(runtime.render()), /测试过于频繁/)
    runtime.remote.whalerein.testConnection = async () => ({ ok: false, error: { code: 'gateway/service-unavailable' } })
    await find(runtime.render(), (node) => node.type === UI.Button && text(node) === '测试连接').props.onClick()
    assert.match(text(runtime.render()), /测试接口尚未就绪/)
  } finally {
    await runtime.dispose()
  }
})

test('two devices with the same display name keep distinct short-id headings', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.setStatus({
      ...runtime.getStatus(),
      devices: [
        { deviceId: 'device-alpha1111', displayName: 'MacBook Air', pairedAt: Date.now(), connected: true },
        { deviceId: 'device-bravo2222', displayName: 'MacBook Air', pairedAt: Date.now(), connected: false },
      ],
    })
    await runtime.refresh()
    const alpha = deviceRow(runtime, 'device-alpha1111')
    const bravo = deviceRow(runtime, 'device-bravo2222')
    assert.match(text(alpha), /MacBook Air · alpha1111/)
    assert.match(text(bravo), /MacBook Air · bravo2222/)
    assert.notEqual(text(alpha), text(bravo))
  } finally {
    await runtime.dispose()
  }
})

test('unknown device OS labels, invalid pairing times and missing optional metadata stay out of the row', async () => {
  const runtime = await createSettingsRuntime()
  try {
    runtime.setStatus({
      ...runtime.getStatus(),
      devices: [
        { deviceId: 'device-unknown', os: 'plan9', pairedAt: 'not-a-time', connected: false },
        { deviceId: 'device-linux', os: 'linux', pairedAt: Date.now(), connected: false },
      ],
    })
    await runtime.refresh()
    const tree = runtime.render()
    assert.match(text(tree), /device-unknown/)
    assert.match(text(tree), /device-linux/)
    assert.match(text(tree), /Linux/)
    assert.doesNotMatch(text(tree), /plan9|not-a-time/)
  } finally {
    await runtime.dispose()
  }
})

async function createManagedRuntime(options = {}) {
  const runtime = await createSettingsRuntime({ managed: true, ...options })
  runtime.render()
  await runtime.settle()
  runtime.render()
  await runtime.settle()
  return runtime
}

const managedButton = (runtime, label) => find(runtime.render(), (node) => node.type === UI.Button && text(node) === label)
const managedDisclosure = (runtime, title) => find(runtime.render(), (node) => node.type === UI.DisclosureRow && node.props.title === title)

test('a fresh managed install shows one cloud action and no technical fields or automatic enrollment', async () => {
  const opened = []
  const originalWindow = globalThis.window
  const runtime = await createManagedRuntime()
  try {
    globalThis.window = { ...originalWindow, open: (...args) => { opened.push(args); return null } }
    const tree = runtime.render()
    const rendered = text(tree)
    assert.match(rendered, /连接到 WhaleRein Cloud/)
    assert.match(rendered, /账号登录始终留在浏览器中/)
    assert.equal(managedButton(runtime, '连接到 WhaleRein Cloud').props.variant, 'primary')
    assert.equal(find(tree, (node) => node.type === UI.Menu), undefined)
    for (const label of ['服务器', '主机 ID', '主机 Token', '主机凭据', '服务端口', '传输端口']) {
      assert.equal(find(tree, (node) => node.type === UI.Input && node.props['aria-label'] === label), undefined)
    }
    assert.deepEqual(runtime.calls, [], 'loading the page must not start enrollment')
    assert.deepEqual(opened, [], 'loading the page must not open a browser')
    assert.doesNotMatch(rendered, /生成配对链接|已配对设备/)
  } finally {
    globalThis.window = originalWindow
    await runtime.dispose()
  }
})

test('duplicate clicks start one managed connection and browser blocking leaves a usable approval link', async () => {
  const originalWindow = globalThis.window
  const opened = []
  const runtime = await createManagedRuntime()
  try {
    globalThis.window = { ...originalWindow, open: (...args) => { opened.push(args); return null } }
    let finish
    runtime.remote.whalerein.startConnect = (request) => {
      runtime.calls.push(['startConnect', request])
      return new Promise((resolve) => { finish = resolve })
    }
    const connect = managedButton(runtime, '连接到 WhaleRein Cloud')
    const first = connect.props.onClick()
    const second = connect.props.onClick()
    assert.equal(runtime.calls.length, 1, 'a second click while requesting must not create another intent')
    runtime.setCloudState('awaiting_approval', { approvalUrl: 'https://app.example/approve/request-1' })
    finish({ ok: true, value: { approvalUrl: 'https://app.example/approve/request-1' } })
    await Promise.all([first, second])
    await runtime.settle()
    const tree = runtime.render()
    assert.match(text(tree), /浏览器阻止了新标签页/)
    const link = find(tree, (node) => node.type === 'a' && text(node) === '打开浏览器')
    assert.ok(link)
    assert.equal(link.props.href, 'https://app.example/approve/request-1?locale=zh-CN')
    assert.equal(link.props.target, '_blank')
    assert.equal(opened.length, 1)
  } finally {
    globalThis.window = originalWindow
    await runtime.dispose()
  }
})

test('an explicit connection action opens the reserved browser tab when allowed', async () => {
  const originalWindow = globalThis.window
  const tab = { location: { href: '' }, close() {} }
  const opened = []
  const runtime = await createManagedRuntime()
  try {
    globalThis.window = { ...originalWindow, open: (...args) => { opened.push(args); return tab } }
    runtime.setActionResult('startConnect', { ok: true, value: { approvalUrl: 'https://app.example/approve/request-2' } })
    await managedButton(runtime, '连接到 WhaleRein Cloud').props.onClick()
    await runtime.settle()
    assert.equal(opened.length, 1)
    assert.equal(tab.location.href, 'https://app.example/approve/request-2?locale=zh-CN')
    assert.equal(tab.opener, null)
  } finally {
    globalThis.window = originalWindow
    await runtime.dispose()
  }
})

test('every managed lifecycle state has actionable localized text and no false connected state', async () => {
  const runtime = await createManagedRuntime()
  try {
    const states = ['idle', 'requesting', 'awaiting_approval', 'enrolling', 'recovering',
      'connecting', 'connected', 'reconnecting', 'disconnected', 'denied', 'expired',
      'action_required', 'removing', 'removed']
    for (const state of states) {
      runtime.setCloudState(state, {
        connected: state === 'connected',
        approvalUrl: state === 'awaiting_approval' ? 'https://app.example/approve/x' : '',
      })
      await runtime.refresh()
      const tree = runtime.render()
      const rendered = text(tree)
      assert.match(rendered, new RegExp(runtime.locales.dictionaries.zh[`state_${state}`]), state)
      assert.equal(find(tree, (node) => node.props?.role === 'status') !== undefined, true, state)
      if (state !== 'connected') assert.doesNotMatch(rendered, /这台电脑已连接，可以进行远程控制/)
      if (state === 'awaiting_approval') assert.match(rendered, /打开浏览器/)
      if (['requesting', 'awaiting_approval', 'enrolling', 'recovering'].includes(state)) assert.match(rendered, /取消/)
      if (['denied', 'expired', 'action_required'].includes(state)) assert.match(rendered, /重试/)
    }
  } finally {
    await runtime.dispose()
  }
})

test('active staging stays labeled and switchable when details and Developer tools are collapsed', async () => {
  const runtime = await createManagedRuntime()
  try {
    runtime.setStatus({
      ...runtime.getStatus(),
      state: 'connected',
      connected: true,
      environment: 'staging',
      stagingEnabled: true,
      profileId: 'profile-staging',
      label: 'WhaleRein Cloud Staging',
      profiles: [
        { id: 'profile-production', label: 'WhaleRein Cloud', environment: 'production', active: false, managed: true },
        { id: 'profile-staging', label: 'WhaleRein Cloud Staging', environment: 'staging', active: true, managed: true },
      ],
    })
    await runtime.refresh()
    const tree = runtime.render()
    assert.ok(find(tree, (node) => node.props?.['data-whalerein-staging'] === 'true'), 'staging label is visible')
    assert.match(text(tree), /Staging/)
    assert.equal(managedDisclosure(runtime, '连接详情').props.open, false)
    assert.equal(managedDisclosure(runtime, '开发者工具'), undefined)
    const switchBack = managedButton(runtime, '切换回生产环境')
    assert.ok(switchBack, 'production switch is reachable without Developer tools')
    await switchBack.props.onClick()
    await runtime.settle()
    assert.deepEqual(runtime.calls.filter(([name]) => name === 'switchProfile'), [['switchProfile', 'profile-production']])
  } finally {
    await runtime.dispose()
  }
})

test('enabling staging never activates it and explicit profile selection does', async () => {
  const runtime = await createManagedRuntime({ development: true })
  try {
    assert.match(text(runtime.render()), /开发者工具/)
    runtime.setStatus({
      ...runtime.getStatus(),
      profiles: [
        { id: 'profile-production', label: 'WhaleRein Cloud', environment: 'production', active: true, managed: true },
      ],
    })
    await runtime.refresh()
    runtime.render()
    await managedButton(runtime, '启用 Staging').props.onClick()
    await runtime.settle()
    assert.deepEqual(runtime.calls.map(([name]) => name), ['enableStaging'])
    assert.equal(runtime.getStatus().environment, 'production', 'enabling staging does not switch')
    runtime.setStatus({
      ...runtime.getStatus(),
      stagingEnabled: true,
      profiles: [
        { id: 'profile-production', label: 'WhaleRein Cloud', environment: 'production', active: true, managed: true },
        { id: 'profile-staging', label: 'WhaleRein Cloud Staging', environment: 'staging', active: false, managed: true },
      ],
    })
    await runtime.refresh()
    runtime.render()
    await managedButton(runtime, '使用 Staging').props.onClick()
    await runtime.settle()
    assert.deepEqual(runtime.calls.filter(([name]) => name === 'switchProfile'), [['switchProfile', 'profile-staging']])
  } finally {
    await runtime.dispose()
  }
})

test('disconnect, remote removal and local profile deletion use distinct wording and calls', async () => {
  const runtime = await createManagedRuntime()
  try {
    runtime.setCloudState('connected', { connected: true })
    await runtime.refresh()
    const tree = runtime.render()
    const rendered = text(tree)
    assert.match(rendered, /断开连接只会在本地暂停这台电脑/)
    assert.match(rendered, /移除这台电脑需要新的浏览器授权/)
    const remove = managedButton(runtime, '移除这台电脑')
    assert.ok(remove)
    await findAll(tree, (node) => node.type === UI.Button && text(node) === '断开连接')[0].props.onClick()
    await runtime.settle()
    assert.deepEqual(runtime.calls.map(([name]) => name), ['disconnect'])
    runtime.setCloudState('removing', { connected: true })
    await runtime.refresh()
    const removing = text(runtime.render())
    assert.match(removing, /正在移除这台电脑/)
    assert.equal(managedButton(runtime, '移除这台电脑').props.disabled, true)
    runtime.setCloudState('removed', { connected: false })
    await runtime.refresh()
    const removed = text(runtime.render())
    assert.match(removed, /这台电脑已被移除/)
    assert.match(removed, /重新添加这台电脑需要新的浏览器授权/)
    assert.equal(managedButton(runtime, '移除这台电脑').props.disabled, true)
    assert.equal(runtime.calls.some(([name]) => name === 'removeComputer'), false)
  } finally {
    await runtime.dispose()
  }
})

test('managed state changes announce through role=status and move focus to the status region', async () => {
  const originalDocument = globalThis.document
  const focused = []
  const runtime = await createManagedRuntime()
  try {
    globalThis.document = { getElementById: (id) => ({ focus: () => focused.push(id) }) }
    const tree = runtime.render()
    const status = find(tree, (node) => node.props?.role === 'status')
    assert.ok(status)
    assert.equal(status.props['aria-live'], 'assertive')
    assert.equal(status.props['aria-atomic'], 'true')
    assert.equal(status.props.tabIndex, -1)
    runtime.setCloudState('awaiting_approval', { approvalUrl: 'https://app.example/approve/x' })
    await runtime.refresh()
    runtime.render()
    assert.ok(focused.length >= 1)
    assert.match(focused.at(-1), /-state$/)
    runtime.setCloudState('connected', { connected: true })
    await runtime.refresh()
    runtime.render()
    assert.equal(find(runtime.render(), (node) => node.props?.role === 'status').props['aria-live'], 'polite')
  } finally {
    globalThis.document = originalDocument
    await runtime.dispose()
  }
})

test('connection details are collapsed by default and show only redacted diagnostics', async () => {
  const runtime = await createManagedRuntime()
  try {
    runtime.setCloudState('connected', { connected: true })
    await runtime.refresh()
    assert.equal(managedDisclosure(runtime, '连接详情').props.open, false)
    runtime.setActionResult('details', {
      ok: true,
      value: {
        state: 'connected', environment: 'production', region: 'eu-west', accessOrigin: 'https://access.example',
        revision: 7, lastErrorCode: 'temporarily_unavailable',
        hostId: 'host-private', route: 'mqtts://private.example', token: 'secret-token',
        authorization: { signature: 'private' },
      },
    })
    managedDisclosure(runtime, '连接详情').props.onToggle()
    await runtime.settle()
    await managedButton(runtime, '刷新详情').props.onClick()
    await runtime.settle()
    const rendered = text(runtime.render())
    assert.match(rendered, /eu-west/)
    assert.match(rendered, /https:\/\/access\.example/)
    assert.match(rendered, /temporarily_unavailable/)
    assert.doesNotMatch(rendered, /host-private|private\.example|secret-token|private/)
    assert.doesNotMatch(rendered, /主机 ID|host-private|mqtts:/)
  } finally {
    await runtime.dispose()
  }
})

test('advanced custom services and inactive local profile deletion are explicit manager calls', async () => {
  const runtime = await createManagedRuntime()
  try {
    runtime.setStatus({
      ...runtime.getStatus(),
      profileId: 'profile-production',
      profiles: [
        { id: 'profile-production', label: 'WhaleRein Cloud', environment: 'production', active: true, managed: true },
        { id: 'profile-old', label: 'Old staging', environment: 'staging', active: false, managed: true },
      ],
    })
    await runtime.refresh()
    const tree = runtime.render()
    const label = find(tree, (node) => node.type === UI.Input && node.props['aria-label'] === '服务名称')
    const origin = find(tree, (node) => node.type === UI.Input && node.props['aria-label'] === 'API 来源')
    label.props.onChange({ target: { value: 'Reviewed service' } })
    origin.props.onChange({ target: { value: 'https://service.example' } })
    await managedButton(runtime, '添加自定义服务').props.onClick()
    await runtime.settle()
    assert.deepEqual(runtime.calls.filter(([name]) => name === 'addCustomService'),
      [['addCustomService', { label: 'Reviewed service', apiOrigin: 'https://service.example' }]])
    const deletes = findAll(runtime.render(), (node) => node.type === UI.Button && /删除本地配置/.test(node.props?.['aria-label'] ?? ''))
    assert.equal(deletes.length, 2)
    assert.equal(deletes[0].props.disabled, true, 'active profile cannot be deleted')
    assert.equal(deletes[1].props.disabled, false)
    await deletes[1].props.onClick()
    await runtime.settle()
    assert.deepEqual(runtime.calls.filter(([name]) => name === 'deleteProfile'), [['deleteProfile', 'profile-old']])
  } finally {
    await runtime.dispose()
  }
})

test('a failed managed action is announced without exposing the private failure text', async () => {
  const runtime = await createManagedRuntime()
  try {
    runtime.setCloudState('awaiting_approval', { approvalUrl: 'https://app.example/approve/x' })
    await runtime.refresh()
    runtime.setActionResult('cancel', new Error('private runtime detail'))
    await managedButton(runtime, '取消').props.onClick()
    await runtime.settle()
    const tree = runtime.render()
    const alert = find(tree, (node) => node.props?.role === 'alert')
    assert.equal(text(alert), '无法完成连接操作，请重试。')
    assert.doesNotMatch(text(tree), /private runtime detail/)
  } finally {
    await runtime.dispose()
  }
})

test('an unsafe approval URL is never rendered as an open-browser action', async () => {
  const runtime = await createManagedRuntime()
  try {
    runtime.setCloudState('awaiting_approval', { approvalUrl: 'javascript:alert(1)' })
    await runtime.refresh()
    const tree = runtime.render()
    assert.equal(find(tree, (node) => node.type === 'a' && /打开浏览器/.test(text(node))), undefined)
    assert.doesNotMatch(text(tree), /javascript:/)
  } finally {
    await runtime.dispose()
  }
})

test('staging switching falls back to manager-returned opaque ids when the snapshot has no profile list', async () => {
  const runtime = await createManagedRuntime({ development: true })
  try {
    runtime.setStatus({
      ...runtime.getStatus(),
      profileId: 'production-id-1',
      environment: 'production',
      profiles: [],
      stagingEnabled: false,
    })
    await runtime.refresh()
    runtime.setActionResult('enableStaging', {
      ok: true,
      value: {
        ...runtime.getStatus(), stagingEnabled: true,
        action: { id: 'staging-id-1', label: 'WhaleRein Cloud Staging', environment: 'staging', active: false },
      },
    })
    runtime.render()
    await managedButton(runtime, '启用 Staging').props.onClick()
    await runtime.settle()
    runtime.setStatus({ ...runtime.getStatus(), stagingEnabled: true })
    await runtime.refresh()
    runtime.render()
    await managedButton(runtime, '使用 Staging').props.onClick()
    await runtime.settle()
    assert.deepEqual(runtime.calls.filter(([name]) => name === 'switchProfile'), [['switchProfile', 'staging-id-1']])
    runtime.setStatus({ ...runtime.getStatus(), environment: 'staging', profileId: 'staging-id-1' })
    await runtime.refresh()
    runtime.render()
    await managedButton(runtime, '切换回生产环境').props.onClick()
    await runtime.settle()
    assert.deepEqual(runtime.calls.filter(([name]) => name === 'switchProfile'),
      [['switchProfile', 'staging-id-1'], ['switchProfile', 'production-id-1']])
  } finally {
    await runtime.dispose()
  }
})

test('Remove this computer opens the browser-authorized management URL and stays removing', async () => {
  const originalWindow = globalThis.window
  const runtime = await createManagedRuntime()
  try {
    globalThis.window = { ...originalWindow, open: () => null }
    runtime.setCloudState('connected', { connected: true })
    await runtime.refresh()
    runtime.setActionResult('removeComputer', {
      ok: true,
      value: {
        ...runtime.getStatus(), state: 'removing',
        action: { state: 'removing', url: 'https://app.example/hosts/host-1', remoteRevocation: 'pending' },
      },
    })
    await managedButton(runtime, '移除这台电脑').props.onClick()
    await runtime.settle()
    assert.deepEqual(runtime.calls.filter(([name]) => name === 'removeComputer'), [['removeComputer']])
    runtime.setCloudState('removing', { connected: true })
    await runtime.refresh()
    const tree = runtime.render()
    const link = find(tree, (node) => node.type === 'a' && text(node) === '打开浏览器')
    assert.equal(link.props.href, 'https://app.example/hosts/host-1?locale=zh-CN')
    assert.match(text(tree), /浏览器阻止了新标签页/)
    assert.match(text(tree), /请在浏览器中确认移除/)
    assert.equal(managedButton(runtime, '移除这台电脑').props.disabled, true)
  } finally {
    globalThis.window = originalWindow
    await runtime.dispose()
  }
})
