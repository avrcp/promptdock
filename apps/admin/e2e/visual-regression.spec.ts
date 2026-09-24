import { test, expect, type Page } from '@playwright/test'

const SCENARIO_STORAGE_KEY = 'promptdock-relay-admin:dev-scenario'
const FROZEN_NOW = 1_769_000_000_000

async function prepare(page: Page, scenario: string): Promise<void> {
  await page.addInitScript(
    ({ key, scenarioId, now }) => {
      window.sessionStorage.setItem(key, scenarioId)
      Date.now = () => now
    },
    { key: SCENARIO_STORAGE_KEY, scenarioId: scenario, now: FROZEN_NOW },
  )
}

async function waitForVisualReady(page: Page): Promise<void> {
  await page
    .locator('[data-testid="skeleton"]')
    .waitFor({ state: 'detached' })
    .catch(() => {})
  await page.evaluate(async () => {
    await document.fonts.ready
  })
}

async function captureWorkbench(page: Page, name: string): Promise<void> {
  await waitForVisualReady(page)
  await expect(page).toHaveScreenshot(`${name}.png`, { animations: 'disabled' })
  await page.locator('main').evaluate((element) => element.scrollTo({ top: element.scrollHeight }))
  await expect(page).toHaveScreenshot(`${name}-bottom.png`, { animations: 'disabled' })
}

interface VisualMatrixCase {
  label: string
  path: '/overview' | '/devices' | '/wechat' | '/queue' | '/system'
  scenario: string
  snapshot: string
  viewport: { width: number; height: number }
  captureBottom?: boolean
}

const DESKTOP = { width: 1280, height: 800 }
const TABLET = { width: 768, height: 900 }
const MOBILE = { width: 384, height: 450 }

const VISUAL_MATRIX: readonly VisualMatrixCase[] = [
  {
    label: 'overview desktop',
    path: '/overview',
    scenario: 'healthy',
    snapshot: 'overview',
    viewport: DESKTOP,
    captureBottom: true,
  },
  {
    label: 'overview tablet',
    path: '/overview',
    scenario: 'healthy',
    snapshot: 'overview-tablet-768x900',
    viewport: TABLET,
  },
  {
    label: 'overview mobile',
    path: '/overview',
    scenario: 'healthy',
    snapshot: 'overview-mobile-384x450',
    viewport: MOBILE,
  },
  {
    label: 'overview partial failure',
    path: '/overview',
    scenario: 'partial-failure',
    snapshot: 'overview-partial-failure',
    viewport: DESKTOP,
    captureBottom: true,
  },
  {
    label: 'devices desktop',
    path: '/devices',
    scenario: 'healthy',
    snapshot: 'devices',
    viewport: DESKTOP,
    captureBottom: true,
  },
  {
    label: 'devices tablet',
    path: '/devices',
    scenario: 'healthy',
    snapshot: 'devices-tablet-768x900',
    viewport: TABLET,
  },
  {
    label: 'devices mobile',
    path: '/devices',
    scenario: 'healthy',
    snapshot: 'devices-mobile-384x450',
    viewport: MOBILE,
  },
  {
    label: 'devices gateway degraded',
    path: '/devices',
    scenario: 'gateway-partial-offline',
    snapshot: 'devices-gateway-partial-offline',
    viewport: DESKTOP,
    captureBottom: true,
  },
  {
    label: 'wechat desktop',
    path: '/wechat',
    scenario: 'healthy',
    snapshot: 'wechat',
    viewport: DESKTOP,
    captureBottom: true,
  },
  {
    label: 'wechat tablet',
    path: '/wechat',
    scenario: 'healthy',
    snapshot: 'wechat-tablet-768x900',
    viewport: TABLET,
  },
  {
    label: 'wechat mobile',
    path: '/wechat',
    scenario: 'healthy',
    snapshot: 'wechat-mobile-384x450',
    viewport: MOBILE,
  },
  {
    label: 'wechat disconnected',
    path: '/wechat',
    scenario: 'wechat-disconnected',
    snapshot: 'wechat-disconnected',
    viewport: DESKTOP,
    captureBottom: true,
  },
  {
    label: 'queue desktop',
    path: '/queue',
    scenario: 'healthy',
    snapshot: 'queue',
    viewport: DESKTOP,
    captureBottom: true,
  },
  {
    label: 'queue tablet',
    path: '/queue',
    scenario: 'healthy',
    snapshot: 'queue-tablet-768x900',
    viewport: TABLET,
  },
  {
    label: 'queue mobile',
    path: '/queue',
    scenario: 'healthy',
    snapshot: 'queue-mobile-384x450',
    viewport: MOBILE,
  },
  {
    label: 'queue backlog',
    path: '/queue',
    scenario: 'queue-backlog',
    snapshot: 'queue-backlog',
    viewport: DESKTOP,
    captureBottom: true,
  },
  {
    label: 'system desktop',
    path: '/system',
    scenario: 'healthy',
    snapshot: 'system',
    viewport: DESKTOP,
    captureBottom: true,
  },
  {
    label: 'system tablet',
    path: '/system',
    scenario: 'healthy',
    snapshot: 'system-tablet-768x900',
    viewport: TABLET,
  },
  {
    label: 'system mobile',
    path: '/system',
    scenario: 'healthy',
    snapshot: 'system-mobile-384x450',
    viewport: MOBILE,
  },
  {
    label: 'system database degraded',
    path: '/system',
    scenario: 'database-degraded',
    snapshot: 'system-degraded',
    viewport: DESKTOP,
    captureBottom: true,
  },
]

test.describe('deterministic visual baselines', () => {
  test.use({ viewport: { width: 1280, height: 800 } })

  for (const visualCase of VISUAL_MATRIX) {
    test(`matrix: ${visualCase.label}`, async ({ page }) => {
      await page.emulateMedia({ reducedMotion: 'reduce' })
      await page.setViewportSize(visualCase.viewport)
      await prepare(page, visualCase.scenario)
      await page.goto(visualCase.path)
      await expect(page.locator('main')).toBeVisible()
      if (visualCase.captureBottom) {
        await captureWorkbench(page, visualCase.snapshot)
      } else {
        await waitForVisualReady(page)
        await expect(page).toHaveScreenshot(`${visualCase.snapshot}.png`, {
          animations: 'disabled',
        })
      }
    })
  }

  test('dialog, drawer, long device, disabled actions, and empty variants', async ({ page }) => {
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await prepare(page, 'healthy')
    await page.goto('/devices')
    await page.getByTestId('device-create').click()
    await expect(page.getByTestId('app-dialog')).toBeVisible()
    await waitForVisualReady(page)
    await expect(page).toHaveScreenshot('device-create-dialog.png', {
      animations: 'disabled',
    })

    await page.getByRole('button', { name: '取消' }).click()
    await page.getByTestId('device-details').first().click()
    await expect(page.getByTestId('app-drawer')).toBeVisible()
    await expect(page).toHaveScreenshot('device-detail-drawer.png', {
      animations: 'disabled',
    })

    await page.keyboard.press('Escape')
    await expect(page.getByTestId('app-drawer')).toHaveCount(0)

    const longDeviceName =
      '华东生产集群上海主控工作站超长设备名称用于验证抽屉标题截断与操作入口稳定性第零零一号'
    await page.getByTestId('device-create').click()
    await page.getByTestId('device-create-name').fill(longDeviceName)
    await page.getByRole('button', { name: '创建并显示 Token', exact: true }).click()
    await expect(page.getByTestId('credential-receipt')).toBeVisible()
    await page.getByRole('button', { name: '我已保存，关闭', exact: true }).click()
    await expect(
      page.getByRole('button', { name: `查看设备详情：${longDeviceName}` }),
    ).toBeVisible()
    await page.getByRole('button', { name: `查看设备详情：${longDeviceName}` }).click()
    await expect(page.getByTestId('app-drawer')).toBeVisible()
    await waitForVisualReady(page)
    await expect(page).toHaveScreenshot('device-detail-drawer-long-title.png', {
      animations: 'disabled',
    })

    await page.keyboard.press('Escape')
    await page.getByRole('button', { name: `设备操作：${longDeviceName}` }).click()
    await page.getByRole('menuitem', { name: '撤销设备' }).click()
    await page.getByRole('button', { name: '撤销', exact: true }).click()
    await expect(page.getByRole('button', { name: `设备操作：${longDeviceName}` })).toBeVisible()
    await page.getByRole('button', { name: `设备操作：${longDeviceName}` }).click()
    await expect(
      page.getByRole('menuitem', { name: /该设备当前状态不支持此操作/ }).first(),
    ).toBeVisible()
    await expect(page).toHaveScreenshot('device-menu-disabled-reason.png', {
      animations: 'disabled',
    })

    await prepare(page, 'empty-first-run')
    await page.goto('/overview')
    await waitForVisualReady(page)
    await expect(page.getByText('当前没有 Gateway 连接')).toBeVisible()
    await expect(page).toHaveScreenshot('overview-empty-bare.png', {
      animations: 'disabled',
    })

    await page.goto('/devices')
    await expect(page.getByText('没有匹配的设备')).toBeVisible()
    await expect(page).toHaveScreenshot('devices-empty.png', {
      animations: 'disabled',
    })

    await page.goto('/queue')
    await waitForVisualReady(page)
    await expect(page.getByText('没有匹配的投递')).toBeVisible()
    await expect(page).toHaveScreenshot('queue-empty.png', {
      animations: 'disabled',
    })
  })

  test('queue dead-letter representative state', async ({ page }) => {
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await prepare(page, 'dead-letter')
    await page.goto('/queue?tab=inbound')
    await waitForVisualReady(page)
    await expect(
      page.locator('[data-testid="status-chip"]').filter({ hasText: '死信' }).first(),
    ).toBeVisible()
    await captureWorkbench(page, 'queue-dead-letter')
  })
})
