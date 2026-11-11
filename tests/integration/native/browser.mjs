// One-shot browser checks. Raw browser errors/URLs, traces and screenshots are
// never exported; the caller receives only a bounded stage code.
import fs from 'node:fs'
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
const require = createRequire(import.meta.url)
const { chromium, expect } = require('/opt/native/node_modules/playwright/test')
const input = JSON.parse(fs.readFileSync(0, 'utf8'))
let stage = 'launch'
let browser
let page
let httpStatus
const browserErrors = []
const consoleErrors = []
try {
  browser = await chromium.launch({ headless: true, args: ['--no-sandbox', '--disable-dev-shm-usage'] })
  const context = await browser.newContext({ locale: 'en-US', viewport: { width: 1280, height: 900 } })
  page = await context.newPage()
  page.on('pageerror', error => browserErrors.push({ kind: error.name, modules: [...error.message.matchAll(/@deepseek-ai\/[a-z0-9-]+/g)].map(match => match[0]) }))
  page.on('console', message => {
    if (message.type() !== 'error') return
    const text = message.text()
    const known = ['whalerein', 'configForms', 'remote.settings', 'ui-primitives', 'not found', 'not registered', 'Cannot', 'namespace', 'failed', 'inject', 'service', 'mount', 'module', 'factory', 'Config', 'locale', 'slots']
    consoleErrors.push(known.filter(word => text.includes(word)))
  })
  page.setDefaultTimeout(15000)
  stage = 'web-ready'
  httpStatus = (await page.goto(input.url, { waitUntil: 'domcontentloaded' }))?.status()
  const openSettings = async () => {
    stage = 'settings-launcher'
    await page.locator('[data-slot="sidebar.settings"]').getByRole('button').first().click()
    stage = 'settings-menu'
    await page.getByRole('menuitem', { name: /^Settings$|^设置$/ }).click()
    stage = 'settings-section'
    await page.getByRole('button', { name: 'WhaleRein', exact: true }).click()
    const panel = page.locator('.whalerein-settings')
    await panel.waitFor()
    stage = 'settings-connected'
    await panel.getByRole('heading', { name: /Host connected to server|当前主机已连接服务器/ }).waitFor()
    stage = 'settings-fields'
    const configuration = panel.getByRole('button', { name: /^Connection settings$|^连接配置$/ })
    await expect(configuration).toHaveAttribute('aria-expanded', 'false')
    await configuration.click()
    await panel.getByRole('textbox', { name: /^Server$|^服务器$/ }).waitFor()
  }
  if (input.action === 'claim-owner') {
    stage = 'session-open'
    const workspace = page.locator('[data-row-key^="workspace:"]').first()
    await workspace.waitFor()
    await workspace.hover()
    await workspace.getByRole('button', { name: /New session in|中新建会话/ }).click()
    const row = page.locator('[data-row-key^="session:"][aria-selected=true]')
    await row.waitFor()
    const sessionId = (await row.getAttribute('data-row-key')).slice('session:'.length)
    assert(sessionId.length > 0)
    stage = 'claim-owner'
    // The native UI creates and subscribes to the Session before remote control.
    fs.writeFileSync(input.readyFile + '.tmp', JSON.stringify({ sessionId }), { mode: 0o600 })
    fs.renameSync(input.readyFile + '.tmp', input.readyFile)
    await expect.poll(() => fs.existsSync(input.pendingFile), { timeout: 20000 }).toBe(true)
    await page.getByText('native browser ownership fixture', { exact: true }).first().waitFor()
    await expect(page.locator('[data-approval-key]')).toHaveCount(0)
    fs.writeFileSync(input.checkedFile, 'checked', { mode: 0o600 })
    await expect.poll(() => fs.existsSync(input.doneFile), { timeout: 20000 }).toBe(true)
    await expect(page.locator('[data-approval-key]')).toHaveCount(0)
  } else {
    await openSettings()
    const panel = page.locator('.whalerein-settings')
    const server = panel.getByRole('textbox', { name: /^Server$|^服务器$/ })
    const token = panel.locator('input[type=password]')
    stage = 'secret-field'
    assert.equal(await token.inputValue(), '')
    stage = 'secret-render'
    assert(!(await panel.innerText()).includes(input.hostToken))
    if (input.action === 'settings') {
      stage = 'connection-test'
      await panel.getByRole('button', { name: /Test connection|测试连接/ }).click()
      await panel.getByText(/Connection test passed|连接测试通过/).waitFor()
      stage = 'save'
      await server.fill('localhost')
      await panel.getByRole('button', { name: /Save changes|保存修改|保存更改|保存设置/ }).click()
      await panel.getByText(/Settings saved|设置已保存/).waitFor()
      stage = 'persist-reload'
      await page.reload({ waitUntil: 'domcontentloaded' })
      await openSettings()
      assert.equal(await server.inputValue(), 'localhost')
      assert.equal(await token.inputValue(), '')
    } else if (input.action === 'bad-draft') {
      stage = 'draft-credentials'
      await token.fill('rejected-native-fixture')
      await panel.getByRole('button', { name: /Test connection|测试连接/ }).click()
      await panel.getByText(/Sign-in rejected|登录被拒绝|凭证被拒绝|服务器拒绝/).waitFor()
      await panel.getByText(/Host connected to server|当前主机已连接服务器/).waitFor()
      stage = 'discard-draft'
      await page.reload({ waitUntil: 'domcontentloaded' })
      await openSettings()
      assert.equal(await token.inputValue(), '')
    } else if (input.action === 'invite') {
      stage = 'generate-invitation'
      await panel.getByRole('button', { name: /Generate pairing link|生成配对链接/ }).click()
      const link = panel.locator('code').filter({ hasText: /^whalerein:\/\/pair/ })
      await link.waitFor()
      const uri = await link.textContent()
      assert(uri.startsWith('whalerein://pair?'))
      fs.writeFileSync(input.invitationFile, uri, { mode: 0o600 })
    } else if (input.action === 'revoke') {
      stage = 'device-list'
      await panel.getByRole('button', { name: /Revoke pairing|撤销配对/ }).first().click()
      stage = 'revoke-confirm'
      await panel.getByRole('button', { name: /Revoke this device|确认撤销/ }).click()
      await panel.getByText(/No paired devices yet|还没有已配对设备|暂无已配对设备/).waitFor()
    } else {
      throw new Error('unknown fixture action')
    }
  }
  stage = 'browser-health'
  assert.equal(browserErrors.length, 0)
  process.stdout.write(JSON.stringify({ ok: true, stage: 'complete' }))
} catch {
  const diagnostics = page ? { httpStatus, buttons: await page.getByRole('button').count(),
    sessionRow: input.sessionId ? await page.locator(`[data-row-key="session:${input.sessionId}"]`).count() : undefined,
    treeRows: await page.getByRole('treeitem').count(),
    workspaceRows: await page.locator('[data-row-key^="workspace:"]').count(),
    passwordFields: await page.locator('input[type=password]').count(),
    whalereinPanels: await page.locator('.whalerein-settings').count(),
    settingsSlot: await page.locator('[data-slot="sidebar.settings"]').count(),
    errors: browserErrors, console: consoleErrors, scripts: await page.locator('script').count() } : { errors: browserErrors }
  process.stdout.write(JSON.stringify({ ok: false, stage, diagnostics }))
  process.exitCode = 1
} finally {
  await browser?.close()
}
