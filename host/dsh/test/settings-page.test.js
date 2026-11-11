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
  const state = []
  const effects = []
  let stateIndex = 0
  let effectIndex = 0
  let pending = []
  const React = {
    createElement(type, props, ...children) {
      return { type, props: props ?? {}, children }
    },
    useId() { return 'whalerein-test' },
    useRef(value) { return React.useState(() => ({ current: value }))[0] },
    useState(initial) {
      const index = stateIndex++
      if (state[index] === undefined) state[index] = typeof initial === 'function' ? initial() : initial
      return [state[index], (value) => {
        state[index] = typeof value === 'function' ? value(state[index]) : value
      }]
    },
    useEffect(callback, deps) {
      const index = effectIndex++
      const previous = effects[index]
      const changed = !previous || !deps || deps.length !== previous.deps?.length
        || deps.some((value, i) => !Object.is(value, previous.deps[i]))
      if (changed) pending.push({ index, callback, deps })
    },
  }
  function render(component, props) {
    stateIndex = 0
    effectIndex = 0
    pending = []
    const tree = component(props)
    for (const effect of pending) {
      effects[effect.index]?.cleanup?.()
      effects[effect.index] = { deps: effect.deps, cleanup: effect.callback() }
    }
    return tree
  }
  async function dispose() {
    for (const effect of effects) await effect?.cleanup?.()
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

async function createSettingsRuntime({ connected = true, development = false, remoteFailure = false, describe = savedSecrets, writable = true } = {}) {
  const writes = []
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
      expiresAtUnixMs: 1700000000000,
      fingerprint: 'abcd ef01 2345 6789',
    },
  }
  let currentStatus = {
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
  const form = {
    getSnapshot: () => ({
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
    }),
    subscribe: () => () => {},
    set: async (key, value) => {
      writes.push(['set', key, value])
      return true
    },
    unset: async (key) => {
      writes.push(['unset', key])
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
  const remote = {
    $mount: async (contribution) => {
      registered.push({ contribution })
      return async () => {}
    },
    whalerein: {
      snapshot: async () => remoteFailure
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
    async refresh() {
      await injected.status.refresh()
    },
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
    assert.ok(runtime.writes.some(([op, key]) => op === 'unset' && key === 'access_port'))
    assert.ok(runtime.writes.some(([op, key]) => op === 'unset' && key === 'mqtt_port'))
    assert.ok(runtime.writes.some(([op, key]) => op === 'unset' && key === 'ca_file'))
    assert.ok(runtime.writes.some(([op, key, value]) => op === 'set' && key === 'log_level' && value === 'debug'))
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
