// Cross-operator browser journey for the JRNY cases (JRNY-12).
//
// One action only: open the real WhaleRein settings page, read the paired
// device list (the only place Access exposes a lost device identity), revoke
// one exact device id, and report the public identities observed before and
// after. No token, URI, cookie or raw browser error is exported.
import fs from 'node:fs'
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const { chromium, expect } = require('/opt/native/node_modules/playwright/test')
const input = JSON.parse(fs.readFileSync(0, 'utf8'))
let stage = 'launch'
let browser
let page
try {
  browser = await chromium.launch({ headless: true, args: ['--no-sandbox', '--disable-dev-shm-usage'] })
  const context = await browser.newContext({ locale: 'en-US', viewport: { width: 1280, height: 900 } })
  page = await context.newPage()
  page.setDefaultTimeout(15000)
  stage = 'web-ready'
  await page.goto(input.url, { waitUntil: 'domcontentloaded' })
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
  stage = 'secret-free'
  assert(!(await panel.innerText()).includes(input.hostToken))

  if (input.action !== 'revoke-device') throw new Error('unknown fixture action')
  stage = 'device-list'
  const rows = panel.locator('.whalerein-device-row')
  await expect(rows.first()).toBeVisible()
  const readRows = async () => {
    const collected = []
    for (let index = 0; index < (await rows.count()); index += 1) {
      collected.push((await rows.nth(index).innerText()).replace(/\s+/g, ' ').trim())
    }
    return collected
  }
  const before = await readRows()
  stage = 'lost-device-listed'
  assert(
    before.some((text) => text.includes(input.revokeDeviceId)),
    'the lost device identity is not on the settings page'
  )
  stage = 'revoke-target'
  const target = rows.filter({ hasText: input.revokeDeviceId })
  await expect(target).toHaveCount(1)
  await target.getByRole('button', { name: /Revoke pairing|撤销配对/ }).click()
  await target.getByRole('button', { name: /Revoke this device|确认撤销/ }).click()
  stage = 'revoked'
  await expect(rows.filter({ hasText: input.revokeDeviceId })).toHaveCount(0)
  const remaining = await readRows()
  process.stdout.write(JSON.stringify({ ok: true, stage: 'complete', devices: before, remaining }))
} catch {
  process.stdout.write(JSON.stringify({
    ok: false,
    stage,
    rowCount: page ? await page.locator('.whalerein-device-row').count() : 0,
    panels: page ? await page.locator('.whalerein-settings').count() : 0,
  }))
  process.exitCode = 1
} finally {
  await browser?.close()
}
