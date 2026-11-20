import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'

let loaded
globalThis.window = { __ModuleLoader__: { load(module) { loaded = module } } }
await import('../client.js')

const React = {
  createElement(type, props, ...children) { return { type, props: props ?? {}, children } },
  useState: (value) => [typeof value === 'function' ? value() : value, () => {}],
  useRef: (value) => ({ current: value }),
  useEffect: () => {},
  useId: () => 'test',
}

async function dictionaries() {
  const registered = []
  const plugin = loaded.factory((name) => name === 'react' ? React : {
    Button: 'Button', DisclosureRow: 'DisclosureRow', Menu: 'Menu', Input: 'Input',
    StateDot: 'StateDot', Tag: 'Tag', writeClipboard: async () => true,
    IconCheckCircleOutlineRegular: 'icon', IconWarningOutlineRegular: 'icon',
    IconLinkOutlineRegular: 'icon', IconChevronDownOutlineRegular: 'icon',
    IconSettingsOutlineRegular: 'icon', IconSlidersTwoOutlineRegular: 'icon',
    IconCodeOutlineRegular: 'icon',
  })
  const ctx = {
    locale: {
      bind: () => (key) => key,
      register(namespace, value) { registered.push({ namespace, value }); return () => {} },
    },
    configForms: {
      get: () => ({ getSnapshot: () => ({ status: 'loading' }), subscribe: () => () => {} }),
      developerTools: { enabled: { getSnapshot: () => false, subscribe: () => () => {} } },
    },
    remote: {
      $mount: async () => () => {},
      whalerein: { snapshot: async () => ({ ok: true, value: {} }), invite: async () => ({}), revoke: async () => ({}), testConnection: async () => ({}) },
      settings: { describe: async () => ({ ok: true, value: { namespaces: [] } }) },
    },
    slots: { inject() { return () => {} }, register() { return () => {} } },
    effect(callback) { const dispose = callback(); return typeof dispose === 'function' ? dispose : () => {} },
    inject(_names, callback) { return callback(ctx) },
  }
  await plugin.apply(ctx)
  return registered[0].value
}

test('inline dictionaries stay in sync and locale JSON files hold package metadata only', async () => {
  const { en, zh } = await dictionaries()
  assert.deepEqual(Object.keys(en).sort(), Object.keys(zh).sort())
  for (const key of Object.keys(en)) {
    assert.ok(en[key].length > 0, `empty English locale value for ${key}`)
    assert.ok(zh[key].length > 0, `empty Chinese locale value for ${key}`)
  }
  for (const locale of ['en', 'zh']) {
    const value = JSON.parse(readFileSync(new URL(`../locale/${locale}.json`, import.meta.url), 'utf8'))
    assert.deepEqual(Object.keys(value), ['meta'])
    assert.deepEqual(Object.keys(value.meta).sort(), ['description', 'title'])
  }
})

test('every literal and dynamic settings key has both dictionary entries', async () => {
  const { en, zh } = await dictionaries()
  const source = readFileSync(new URL('../client.js', import.meta.url), 'utf8')
  const keys = new Set()
  for (const match of source.matchAll(/\b(?:tr|t)\('([^']+)'/g)) keys.add(match[1])
  // Dynamic keys passed through helpers or computed test stages.
  for (const key of [
    'test_invalid_config', 'test_timeout', 'test_dns', 'test_refused', 'test_tls',
    'test_not_ready', 'test_network', 'test_busy', 'testRateLimited', 'testCredentials',
    'inviteKey', 'inviteServer', 'inviteUnauthorized', 'inviteHost', 'inviteFailed',
    'revokeKey', 'revokeUnauthorized', 'revokeMissing', 'revokeFailed',
    'server', 'hostId', 'hostToken', 'accessPort', 'transportPort', 'caFile', 'logLevel',
    'serverSubtitle', 'hostIdSubtitle', 'hostTokenSubtitle', 'accessPortSubtitle',
    'transportPortSubtitle', 'caFileSubtitle', 'logLevelSubtitle',
    'serverPlaceholder', 'hostIdPlaceholder', 'tokenPlaceholder', 'accessPortPlaceholder',
    'transportPortPlaceholder', 'caFilePlaceholder',
    'errorServerRequired', 'errorServerInvalid', 'errorPortInvalid', 'errorHostIdInvalid',
    'errorLogLevelInvalid', 'errorSaveConflict', 'uriCopied', 'copyFailed',
    'testStageConfiguration', 'testStageAccess', 'testStageMqtt',
  ]) keys.add(key)
  for (const key of keys) {
    if (key.includes('${')) continue
    assert.ok(key in en, `missing English key ${key}`)
    assert.ok(key in zh, `missing Chinese key ${key}`)
  }
})


test('managed action labels and terminal reasons are localized in both languages', async () => {
  const { en, zh } = await dictionaries()
  const keys = [
    // Actions and setup entry.
    'cloudConnect', 'connectCustom', 'cloudResume', 'cloudResuming', 'cloudRecoverCredential',
    'cloudRecoveringCredential', 'cloudVerifyStatus', 'cloudOpenBrowser', 'cloudCancel',
    'cloudTryAgain', 'cloudCancelRemoval', 'cloudContinueSetup',
    // Setup and removal progress.
    'cloudConfirmationRequired', 'cloudRemovalVerified', 'cloudEnforcementConfirmed',
    'cloudBrowserBlocked',
    // Transport, credential and TLS terminal reasons.
    'errorHint_transport', 'errorHint_credential', 'errorHint_unauthorized',
    'errorHint_tls_trust', 'errorHint_invalid_config', 'errorHint_recovery_required',
    'errorHint_removal_pending',
    'configurationConflict',
  ]
  for (const key of keys) {
    assert.ok(en[key]?.length > 0, `missing English ${key}`)
    assert.ok(zh[key]?.length > 0, `missing Chinese ${key}`)
  }
  // Recovery and TLS wording names the correct setting in both languages.
  assert.match(en.errorHint_tls_trust, /CA provider|CA file/)
  assert.match(zh.errorHint_tls_trust, /CA/)
  assert.match(en.errorHint_credential, /Host Token|credential recovery/)
  assert.match(zh.errorHint_credential, /主机 Token|凭据恢复/)
  assert.match(en.configurationConflict, /process|identity/)
  assert.match(zh.configurationConflict, /进程|身份/)
})
