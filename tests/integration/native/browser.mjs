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
  browser = await chromium.launch({ headless: true, executablePath: process.env.WHALEREIN_CHROMIUM_PATH || undefined, args: ['--no-sandbox', '--disable-dev-shm-usage'] })
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
      stage = 'service-selector'
      const selector = panel.getByRole('button', { name: /^Connection type$|^连接方式$/ })
      stage = 'service-selector-open'
      await selector.click()
      stage = 'service-selector-choice'
      await page.getByText('Standalone', { exact: true }).last().click()
      stage = 'service-selector-save'
      await panel.getByRole('button', { name: /^Save connection$|^保存连接配置$/ }).click()
      stage = 'service-selector-saved'
      const savedConnection = panel.getByText(/Saved. Reload the plugin|已保存，请重新加载插件/)
      const conflict = panel.getByText(/Settings changed in another window|设置已在另一个窗口修改/)
      await expect.poll(async () => (await savedConnection.count()) + (await conflict.count())).toBeGreaterThan(0)
      if (await conflict.count()) {
        // Exercise explicit user recovery when the native namespace changes
        // after the first describe; the product never retries silently.
        stage = 'service-reload-values'
        await panel.getByRole('button', { name: /^Load saved values$|^载入已保存的值$/ }).click()
        await panel.getByRole('button', { name: /^Save connection$|^保存连接配置$/ }).click()
      }
      stage = 'service-selector-confirmed'
      await savedConnection.waitFor()
      assert(!/Enable staging|启用 Staging|Use local development|使用本地开发/.test(await panel.innerText()))
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
    } else if (input.action === 'settings-advanced') {
      stage = 'advanced-open'
      await panel.getByRole('button', { name: /^Advanced$|^高级配置$/ }).click()
      const servicePort = panel.getByRole('textbox', { name: /^Service port$|^服务端口$/ })
      const transportPort = panel.getByRole('textbox', { name: /^Transport port$|^传输端口$/ })
      const caBundle = panel.getByRole('textbox', { name: /^CA bundle$|^CA 证书路径$/ })
      await servicePort.waitFor(); await transportPort.waitFor(); await caBundle.waitFor()
      stage = 'advanced-values'
      await servicePort.fill('8443')
      await transportPort.fill('8884')
      await caBundle.fill('/etc/whalerein/missing-ca.pem')
      assert.equal(await servicePort.inputValue(), '8443')
      assert.equal(await transportPort.inputValue(), '8884')
      assert.equal(await caBundle.inputValue(), '/etc/whalerein/missing-ca.pem')
      stage = 'network-failure'
      // Keep the draft otherwise valid: a missing CA path would fail the
      // configuration stage before DNS is ever attempted.
      await caBundle.fill('')
      await server.fill('no-such-host.invalid')
      await panel.getByRole('button', { name: /Test connection|测试连接/ }).click()
      await panel.getByText(/Hostname could not be resolved|Network connection failed|无法解析服务器地址|网络连接失败/).first().waitFor()
      const failureText = await panel.innerText()
      assert(!/ENOTFOUND|ETIMEDOUT|ECONNREFUSED|MQTT URL|mqtt:\/\//.test(failureText))
      await server.fill('')
    } else if (input.action === 'settings-layout') {
      stage = 'narrow-layout'
      await page.setViewportSize({ width: 390, height: 844 })
      const narrow = await page.evaluate(() => {
        const panel = document.querySelector('.whalerein-settings')
        return {
          scrollWidth: document.documentElement.scrollWidth,
          innerWidth: window.innerWidth,
          panelRight: panel.getBoundingClientRect().right,
          panelScrollWidth: panel.scrollWidth,
          panelClientWidth: panel.clientWidth,
          media: window.matchMedia('(max-width: 560px)').matches,
        }
      })
      assert(narrow.scrollWidth <= narrow.innerWidth + 1, 'document must not overflow at 390px')
      assert(narrow.panelRight <= 391, 'WhaleRein panel must fit the narrow viewport')
      assert(narrow.panelScrollWidth <= narrow.panelClientWidth + 1, 'panel must not scroll horizontally')
      const fieldRows = panel.locator('.whalerein-field-row')
      if ((await fieldRows.count()) === 0) {
        // The settings hub may restore a collapsed disclosure; open it so the
        // layout assertions observe real field rows rather than an empty panel.
        await panel.getByRole('button', { name: /^Connection settings$|^连接配置$/ }).click()
      }
      await expect(fieldRows.first()).toBeVisible({ timeout: 15000 })
      const fieldRow = fieldRows.first()
      const columns = await fieldRow.evaluate((element) => getComputedStyle(element).gridTemplateColumns)
      assert.equal(
        columns.trim().split(/\s+/).length,
        1,
        `field rows collapse to one column (innerWidth=${narrow.innerWidth}, media=${narrow.media}, columns=${columns})`,
      )
      stage = 'theme-dark'
      const colors = () => page.evaluate(() => {
        const panel = document.querySelector('.whalerein-settings')
        const root = getComputedStyle(document.documentElement)
        return {
          panelColor: getComputedStyle(panel).color,
          background: getComputedStyle(document.body).backgroundColor,
          alias: root.getPropertyValue('--dsw-alias-bg-base').trim(),
          scheme: root.colorScheme,
        }
      })
      await page.emulateMedia({ colorScheme: 'dark' })
      await page.waitForTimeout(250)
      const dark = await colors()
      await page.emulateMedia({ colorScheme: 'light' })
      await page.waitForTimeout(250)
      const light = await colors()
      assert(dark.panelColor !== light.panelColor || dark.background !== light.background || dark.alias !== light.alias,
        `theme tokens did not change: dark=${JSON.stringify(dark)} light=${JSON.stringify(light)}`)
      stage = 'keyboard-disclosure'
      await page.setViewportSize({ width: 1280, height: 900 })
      const connection = panel.getByRole('button', { name: /^Connection settings$|^连接配置$/ })
      await expect(connection).toHaveAttribute('aria-expanded', 'true')
      await connection.focus()
      await page.keyboard.press('Enter')
      await expect(connection).toHaveAttribute('aria-expanded', 'false')
      await page.keyboard.press('Enter')
      await expect(connection).toHaveAttribute('aria-expanded', 'true')
      stage = 'keyboard-advanced-menu'
      const advanced = panel.getByRole('button', { name: /^Advanced$|^高级配置$/ })
      await expect(advanced).toHaveAttribute('aria-expanded', 'false')
      await advanced.focus()
      await page.keyboard.press('Enter')
      await expect(advanced).toHaveAttribute('aria-expanded', 'true')
      const menu = panel.getByRole('button', { name: /^(?:Log level|日志级别)(?:\s|$)/ })
      await menu.focus()
      await page.keyboard.press('Enter')
      await page.getByRole('menuitem').first().waitFor()
      await page.keyboard.press('Escape')
      await expect(menu).toHaveAttribute('aria-expanded', 'false')
      stage = 'keyboard-revoke'
      if (input.deviceId) {
        const revoke = panel.getByRole('button', { name: /Revoke pairing|撤销配对/ }).first()
        await revoke.focus()
        await page.keyboard.press('Enter')
        const cancel = panel.getByRole('button', { name: /^Cancel$|^取消$/ })
        await cancel.waitFor()
        const cancelFocused = await page.evaluate(() => /Cancel|取消/.test(document.activeElement?.textContent ?? ''))
        assert(cancelFocused, 'revoke confirmation must focus its first control')
        await page.keyboard.press('Escape')
        await expect(cancel).toHaveCount(0)
        const triggerFocused = await page.evaluate(() => /Revoke pairing|撤销配对/.test(document.activeElement?.textContent ?? ''))
        assert(triggerFocused, 'focus must return to the revoke trigger after cancellation')
      }
    } else {
      throw new Error('unknown fixture action')
    }
  }
  stage = 'browser-health'
  assert.equal(browserErrors.length, 0)
  process.stdout.write(JSON.stringify({ ok: true, stage: 'complete' }))
} catch (error) {
  const diagnostics = page ? { detail: String(error?.message ?? error).slice(0, 400), httpStatus, buttons: await page.getByRole('button').count(),
    sessionRow: input.sessionId ? await page.locator(`[data-row-key="session:${input.sessionId}"]`).count() : undefined,
    treeRows: await page.getByRole('treeitem').count(),
    workspaceRows: await page.locator('[data-row-key^="workspace:"]').count(),
    fieldRows: await page.locator('.whalerein-field-row').count(),
    passwordFields: await page.locator('input[type=password]').count(),
    whalereinPanels: await page.locator('.whalerein-settings').count(),
    settingsSlot: await page.locator('[data-slot="sidebar.settings"]').count(),
    errors: browserErrors, console: consoleErrors, scripts: await page.locator('script').count() } : { errors: browserErrors }
  process.stdout.write(JSON.stringify({ ok: false, stage, diagnostics }))
  process.exitCode = 1
} finally {
  await browser?.close()
}
