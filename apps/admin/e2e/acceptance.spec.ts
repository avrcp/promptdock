import { test, expect, type Page } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

const SCENARIO_STORAGE_KEY = 'promptdock-relay-admin:dev-scenario'
const FROZEN_NOW = 1_769_000_000_000

async function useScenario(page: Page, scenario: string): Promise<void> {
  await page.addInitScript(
    ({ key, scenarioId, now }) => {
      window.sessionStorage.setItem(key, scenarioId)
      Date.now = () => now
    },
    { key: SCENARIO_STORAGE_KEY, scenarioId: scenario, now: FROZEN_NOW },
  )
}

async function expectNoSeriousAxe(page: Page): Promise<void> {
  const results = await new AxeBuilder({ page }).analyze()
  expect(
    results.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical'),
  ).toEqual([])
}

async function expectNoBodyOverflow(page: Page): Promise<void> {
  const result = await page.evaluate(() => ({
    overflow: document.documentElement.scrollWidth > window.innerWidth,
    viewport: window.innerWidth,
    scrollWidth: document.documentElement.scrollWidth,
    offenders: Array.from(document.querySelectorAll<HTMLElement>('body *'))
      .map((element) => ({
        tag: element.tagName.toLowerCase(),
        className: element.className,
        right: Math.round(element.getBoundingClientRect().right),
        width: Math.round(element.getBoundingClientRect().width),
      }))
      .filter((entry) => entry.right > window.innerWidth + 1)
      .slice(0, 8),
  }))
  expect(result.overflow, JSON.stringify(result)).toBe(false)
}

interface QueueLoadingAudit {
  seen: boolean
  showedEmptyCopy: boolean
  maxLiveAnnouncements: number
  skeletonsWereDecorative: boolean
  refreshSeen: boolean
  refreshRowCount: number
}

async function observeInitialQueueLoading(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const audit: QueueLoadingAudit = {
      seen: false,
      showedEmptyCopy: false,
      maxLiveAnnouncements: 0,
      skeletonsWereDecorative: false,
      refreshSeen: false,
      refreshRowCount: 0,
    }
    ;(
      window as typeof window & {
        __queueLoadingAudit?: QueueLoadingAudit
      }
    ).__queueLoadingAudit = audit

    const scan = () => {
      const loading = document.querySelector('[data-testid="queue-skeleton"]')
      if (loading) {
        audit.seen = true
        const pageText = document.body.textContent ?? ''
        audit.showedEmptyCopy ||=
          pageText.includes('暂无数据') || pageText.includes('没有匹配的投递')
        audit.maxLiveAnnouncements = Math.max(
          audit.maxLiveAnnouncements,
          document.querySelectorAll('main [role="status"][aria-live="polite"]').length,
        )
        const skeletons = Array.from(loading.querySelectorAll('[data-testid="skeleton"]'))
        audit.skeletonsWereDecorative =
          skeletons.length === 4 &&
          skeletons.every(
            (skeleton) =>
              skeleton.getAttribute('aria-hidden') === 'true' && !skeleton.hasAttribute('role'),
          )
      }

      if (document.querySelector('[data-testid="delivery-table"] .app-table__loading-rail')) {
        audit.refreshSeen = true
        audit.refreshRowCount = Math.max(
          audit.refreshRowCount,
          document.querySelectorAll('[data-testid="delivery-table"] tbody tr').length,
        )
      }
    }

    const nativeInsertBefore = Node.prototype.insertBefore
    Node.prototype.insertBefore = function <T extends Node>(
      newNode: T,
      referenceNode: Node | null,
    ): T {
      const inserted = nativeInsertBefore.call(this, newNode, referenceNode) as T
      scan()
      return inserted
    }

    new MutationObserver(scan).observe(document.documentElement, {
      childList: true,
      subtree: true,
    })
  })
}

test.describe('0.6.0-rc.1 frontend acceptance', () => {
  test('overview scan, partial failure, and responsive viewports remain usable', async ({
    page,
  }) => {
    await useScenario(page, 'healthy')
    await page.setViewportSize({ width: 1920, height: 1080 })
    await page.goto('/overview')
    await expect(page.getByRole('heading', { name: '总览' })).toBeVisible()
    await expect(page.getByTestId('stat-relay')).toContainText('正常')
    await expect(page.getByTestId('stat-wechat')).toContainText('已就绪')
    await expectNoSeriousAxe(page)

    for (const viewport of [
      { width: 768, height: 900 },
      { width: 900, height: 900 },
      { width: 1024, height: 900 },
      { width: 1280, height: 800 },
      { width: 1440, height: 900 },
      { width: 1920, height: 1080 },
    ]) {
      await page.setViewportSize(viewport)
      await expect(page.locator('main')).toBeVisible()
      await expectNoBodyOverflow(page)
    }

    await useScenario(page, 'partial-failure')
    await page.goto('/overview')
    await expect(page.getByTestId('wechat-error')).toBeVisible()
    await expect(page.getByTestId('stat-relay')).toBeVisible()
  })

  test('every operational route owns overflow across the production viewport matrix', async ({
    page,
  }) => {
    await useScenario(page, 'healthy')
    const routes = [
      { path: '/overview', heading: '总览' },
      { path: '/devices', heading: '设备' },
      { path: '/wechat', heading: '微信通道' },
      { path: '/queue', heading: '队列' },
      { path: '/system', heading: '系统' },
    ]
    const viewports = [
      { width: 768, height: 900 },
      { width: 900, height: 900 },
      { width: 1024, height: 900 },
      { width: 1280, height: 800 },
      { width: 1440, height: 900 },
      { width: 1920, height: 1080 },
    ]

    for (const viewport of viewports) {
      await page.setViewportSize(viewport)
      for (const route of routes) {
        await page.goto(route.path)
        await expect(page.getByRole('heading', { name: route.heading, exact: true })).toBeVisible()
        await expect(page.locator('#app-route-title')).toHaveText(route.heading)
        await expect(page.locator('h1')).toHaveCount(1)
        await expect(page.locator('main')).toHaveAttribute('aria-labelledby', 'app-route-title')
        await expect(page.locator('.page-header')).toHaveCount(0)
        await expectNoBodyOverflow(page)
      }
    }
  })

  test('media modes and 200% zoom keep the primary shell usable', async ({ page }) => {
    await useScenario(page, 'healthy')
    await page.setViewportSize({ width: 768, height: 900 })
    await page.emulateMedia({ reducedMotion: 'reduce', forcedColors: 'active' })
    await page.goto('/overview')
    await expect(page.getByRole('heading', { name: '总览' })).toBeVisible()
    await expect(page.getByTestId('env-badge-mock')).toBeVisible()

    // Chromium exposes no stable browser-chrome zoom API. A 768×900 physical
    // viewport at 200% zoom has an effective 384×450 CSS viewport; using that
    // viewport also triggers the same reflow media queries as real zoom.
    await page.setViewportSize({ width: 384, height: 450 })
    for (const route of [
      { path: '/overview', heading: '总览' },
      { path: '/devices', heading: '设备' },
      { path: '/wechat', heading: '微信通道' },
      { path: '/queue', heading: '队列' },
      { path: '/system', heading: '系统' },
    ]) {
      await page.goto(route.path)
      await expect(page.locator('main')).toBeVisible()
      await expect(page.getByRole('heading', { name: route.heading, exact: true })).toBeVisible()
      await expectNoBodyOverflow(page)
    }
  })

  test('devices support keyboard-safe details, destructive confirmation, and focus restoration', async ({
    page,
  }) => {
    await useScenario(page, 'healthy')
    await page.goto('/devices')
    await expect(page.getByRole('heading', { name: '设备' })).toBeVisible()
    await page.getByTestId('device-search').fill('主控')
    await expect(page.locator('[data-testid="app-table"] tbody tr')).toHaveCount(1)

    const detail = page.getByTestId('device-details').first()
    await detail.focus()
    await detail.press('Enter')
    const drawer = page.getByTestId('app-drawer').getByRole('dialog')
    await expect(drawer).toContainText('mock-uuid-a1b2c3d4')
    await expect(page).toHaveURL(/device=/)
    await page.goBack()
    await expect(page.getByTestId('app-drawer')).toHaveCount(0)
    await page.goForward()
    await expect(page.getByTestId('app-drawer')).toContainText('主控工作站')
    await page.keyboard.press('Escape')
    await expect(page.getByTestId('app-drawer')).toHaveCount(0)
    await expect(detail).toBeFocused()

    await page
      .getByRole('button', { name: /设备操作：/ })
      .first()
      .click()
    await page.getByRole('menuitem', { name: '撤销设备' }).click()
    const dialog = page.getByTestId('app-dialog').getByRole('dialog')
    await expect(dialog).toContainText('撤销设备')
    await page.keyboard.press('Escape')
    await expect(page.getByTestId('app-dialog')).toHaveCount(0)

    const actionTrigger = page.getByRole('button', { name: /设备操作：/ }).first()
    await actionTrigger.click()
    await page.getByRole('menuitem', { name: '撤销设备' }).click()
    await page.getByTestId('app-dialog').getByRole('button', { name: '撤销', exact: true }).click()
    await expect(page.getByTestId('app-dialog')).toHaveCount(0)
    await expect(
      page.getByTestId('status-chip').filter({ hasText: '已撤销' }).first(),
    ).toBeVisible()

    await actionTrigger.focus()
    await actionTrigger.press('ArrowDown')
    const disabledAction = page.getByRole('menuitem', { name: /Rotate Token/ })
    await page.keyboard.press('ArrowDown')
    await expect(disabledAction).toBeFocused()
    await expect(disabledAction).toHaveAttribute('aria-disabled', 'true')
    const reasonId = await disabledAction.getAttribute('aria-describedby')
    expect(reasonId).toBeTruthy()
    await expect(page.locator(`#${reasonId}`)).toContainText('当前状态不支持此操作')
    await disabledAction.press('Enter')
    await expect(page.getByRole('menu')).toBeVisible()
    await expect(page.getByTestId('app-dialog')).toHaveCount(0)
    await expectNoSeriousAxe(page)
  })

  test('mobile dialogs keep visual and Tab order aligned, and long drawer titles preserve close', async ({
    page,
  }) => {
    const longName = '超长设备名称用于验证抽屉标题不会遮挡关闭按钮并保持移动端布局稳定'.repeat(2)
    await useScenario(page, 'healthy')
    await page.setViewportSize({ width: 320, height: 720 })
    await page.goto('/devices')
    await page.getByTestId('device-create').click()

    const createDialog = page.getByTestId('app-dialog').getByRole('dialog')
    const cancel = createDialog.getByRole('button', { name: '取消', exact: true })
    const primary = createDialog.getByRole('button', { name: '创建并显示 Token', exact: true })
    await expect(cancel).toBeFocused()
    const cancelBox = await cancel.boundingBox()
    const primaryBox = await primary.boundingBox()
    expect(cancelBox).not.toBeNull()
    expect(primaryBox).not.toBeNull()
    expect(cancelBox!.y).toBeLessThan(primaryBox!.y)
    await page.keyboard.press('Tab')
    await expect(primary).toBeFocused()

    await page.getByTestId('device-create-name').fill(longName)
    await primary.click()
    const receipt = page.getByTestId('credential-receipt')
    await expect(receipt).toBeVisible()
    await receipt.getByRole('button', { name: '关闭一次性凭证对话框' }).click()

    await page.getByRole('button', { name: `查看设备详情：${longName}` }).click()
    const drawer = page.getByTestId('app-drawer')
    const drawerTitle = drawer.locator('.app-drawer__title')
    const drawerClose = drawer.getByRole('button', { name: '关闭抽屉' })
    await expect(drawerTitle).toHaveText(longName)
    await expect(drawerClose).toBeVisible()
    const titleAndClose = await drawer.evaluate((root) => {
      const title = root.querySelector<HTMLElement>('.app-drawer__title')
      const close = root.querySelector<HTMLElement>('.app-drawer__close')
      if (!title || !close) throw new Error('drawer title or close button missing')
      const titleRect = title.getBoundingClientRect()
      const closeRect = close.getBoundingClientRect()
      return {
        titleRight: titleRect.right,
        closeLeft: closeRect.left,
        titleOverflows: title.scrollWidth > title.clientWidth,
      }
    })
    expect(titleAndClose.titleRight).toBeLessThanOrEqual(titleAndClose.closeLeft)
    expect(titleAndClose.titleOverflows).toBe(true)
    await expectNoBodyOverflow(page)
  })

  test('queue distinguishes initial loading from empty and preserves rows during refresh', async ({
    page,
  }) => {
    await observeInitialQueueLoading(page)
    await useScenario(page, 'slow-network')
    await page.goto('/queue')
    const deliveryRows = page.getByTestId('delivery-table').locator('tbody tr')
    await expect(deliveryRows).toHaveCount(2)
    const loadingAudit = await page.evaluate(
      () =>
        (
          window as typeof window & {
            __queueLoadingAudit?: QueueLoadingAudit
          }
        ).__queueLoadingAudit,
    )
    expect(loadingAudit).toEqual({
      seen: true,
      showedEmptyCopy: false,
      maxLiveAnnouncements: 1,
      skeletonsWereDecorative: true,
      refreshSeen: false,
      refreshRowCount: 0,
    })

    await page.getByTestId('queue-refresh').click()
    await expect(deliveryRows).toHaveCount(2)
    await expect(page.getByTestId('delivery-table')).toBeVisible()
    const refreshAudit = await page.evaluate(
      () =>
        (
          window as typeof window & {
            __queueLoadingAudit?: QueueLoadingAudit
          }
        ).__queueLoadingAudit,
    )
    expect(refreshAudit?.refreshSeen).toBe(true)
    expect(refreshAudit?.refreshRowCount).toBe(2)
  })

  test('empty queue reports a zero total without an invalid pagination range', async ({ page }) => {
    await useScenario(page, 'empty-first-run')
    await page.goto('/queue')
    await expect(page.getByText('共 0 条', { exact: true })).toBeVisible()
    await expect(page.getByText('没有匹配的投递', { exact: true })).toBeVisible()
    await expect(page.getByText(/第 1[–-]0 条/)).toHaveCount(0)
  })

  test('wechat login clears its in-memory QR snapshot on cancellation and scopes test acceptance', async ({
    page,
  }) => {
    await useScenario(page, 'wechat-disconnected')
    await page.goto('/wechat')
    const timestampValues = page.locator('.wechat-status__detail dd').filter({ hasText: '—' })
    await expect(timestampValues).toHaveCount(3)
    await page.getByTestId('wechat-login').click()
    await expect(page.getByTestId('wechat-qr')).toBeVisible()
    const qrPayload = 'https://mock.promptdock.invalid/login/demo-session'
    expect(await page.content()).not.toContain(qrPayload)
    await page.getByRole('button', { name: '取消登录' }).click()
    await expect(page.getByTestId('app-dialog')).toHaveCount(0)
    await expect(page.getByTestId('wechat-qr')).toHaveCount(0)
    expect(await page.content()).not.toContain(qrPayload)
    await page.getByTestId('wechat-test').click()
    await expect(page.getByTestId('inline-alert')).toContainText('Relay 已接管')
    await expectNoSeriousAxe(page)
  })

  test('CopyValue exposes a visible failure when the browser clipboard rejects the write', async ({
    page,
  }) => {
    await page.addInitScript(() => {
      Object.defineProperty(navigator, 'clipboard', {
        configurable: true,
        value: {
          writeText: () => Promise.reject(new DOMException('clipboard denied', 'NotAllowedError')),
        },
      })
    })
    await useScenario(page, 'healthy')
    await page.goto('/wechat')
    await page.getByRole('button', { name: '复制', exact: true }).click()
    await expect(page.locator('.copy-value__error')).toHaveText('复制失败')
    await expect(page.locator('.copy-value__error')).toBeVisible()
  })

  test('long status labels stay contained without causing body overflow', async ({ page }) => {
    await useScenario(page, 'database-degraded')
    await page.setViewportSize({ width: 320, height: 720 })
    await page.goto('/system')
    const chip = page.getByTestId('status-chip').first()
    await chip.locator('.status-chip__label').evaluate((label) => {
      label.textContent = '异常状态标签'.repeat(30)
    })
    const dimensions = await chip.evaluate((element) => ({
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
      parentWidth: element.parentElement?.clientWidth ?? 0,
    }))
    expect(dimensions.clientWidth).toBeLessThanOrEqual(dimensions.parentWidth)
    expect(dimensions.scrollWidth).toBeLessThanOrEqual(dimensions.clientWidth)
    await expectNoBodyOverflow(page)
  })

  test('reduced motion removes transition duration and press transforms in computed styles', async ({
    page,
  }) => {
    await useScenario(page, 'healthy')
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.goto('/queue')
    const refresh = page.getByTestId('queue-refresh')
    await expect(refresh).toBeVisible()
    const transitionDurations = await refresh.evaluate((element) =>
      getComputedStyle(element)
        .transitionDuration.split(',')
        .map((duration) => duration.trim()),
    )
    expect(transitionDurations.every((duration) => duration === '0s')).toBe(true)
    const box = await refresh.boundingBox()
    expect(box).not.toBeNull()
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2)
    await page.mouse.down()
    expect(await refresh.evaluate((element) => getComputedStyle(element).transform)).toBe('none')
    await page.mouse.up()
  })

  test('queue filtering, explicit detail entry, URL history, and dead-letter state remain safe', async ({
    page,
  }) => {
    await useScenario(page, 'queue-backlog')
    await page.goto('/queue')
    await page.getByTestId('delivery-state-filter').selectOption('retrying')
    await expect(page.getByTestId('delivery-table').locator('tbody tr')).toHaveCount(2)
    await expect(
      page.getByTestId('delivery-table').locator('tbody tr').first(),
    ).not.toHaveAttribute('tabindex')
    const detail = page.getByTestId('delivery-table').locator('.queue__detail-link').first()
    await detail.focus()
    await detail.press('Enter')
    await expect(page.getByTestId('app-drawer')).toContainText('条目详情')
    await expect(page).toHaveURL(/item=/)
    await page.goBack()
    await expect(page.getByTestId('app-drawer')).toHaveCount(0)
    await page.goForward()
    await expect(page.getByTestId('app-drawer')).toContainText('条目详情')
    await page.goBack()
    await expect(page.getByTestId('app-drawer')).toHaveCount(0)

    await page.goto('/queue?tab=replies')
    await expect(page.getByTestId('reply-table')).toBeVisible()

    await useScenario(page, 'dead-letter')
    await page.goto('/queue')
    await page.getByRole('tab', { name: '入站命令' }).click()
    await expect(page.getByTestId('inbound-table')).toContainText('死信')
    await expect(page.getByTestId('inbound-table')).not.toContainText('pdv2.')
    await expectNoSeriousAxe(page)
  })

  test('queue priority is split into a text level and numeric value, with contained origin text', async ({
    page,
  }) => {
    await useScenario(page, 'queue-backlog')
    await page.setViewportSize({ width: 1280, height: 800 })
    await page.goto('/queue')
    const table = page.getByTestId('delivery-table')
    await expect(table.locator('th')).toContainText(['优先级', '值'])
    const headerTexts = await table.locator('thead th').allTextContents()
    const priorityIndex = headerTexts.findIndex((text) => text.trim() === '优先级')
    const valueIndex = headerTexts.findIndex((text) => text.trim() === '值')
    expect(priorityIndex).toBeGreaterThanOrEqual(0)
    expect(valueIndex).toBeGreaterThan(priorityIndex)
    const firstRow = table.locator('tbody tr').first()
    await expect(firstRow.locator('td').nth(priorityIndex)).toContainText(/紧急|高|普通|最低/)
    await expect(firstRow.locator('td').nth(valueIndex)).toHaveText(/^\d+$/)
    const origin = firstRow.locator('.queue-table__origin')
    await expect(origin).toHaveCSS('text-overflow', 'ellipsis')
    await expect(origin).toHaveCSS('white-space', 'nowrap')
  })

  test('system uses the compact six-panel geometry at 1440px', async ({ page }) => {
    await useScenario(page, 'healthy')
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.goto('/system')
    const geometry = await page.evaluate(() => {
      const get = (selector: string) => {
        const element = document.querySelector<HTMLElement>(selector)
        if (!element) throw new Error(`missing ${selector}`)
        const rect = element.getBoundingClientRect()
        return { x: rect.x, y: rect.y, width: rect.width }
      }
      return {
        build: get('.system__card--build'),
        database: get('.system__card--database'),
        retention: get('.system__card--retention'),
        workers: get('.system__card--workers'),
        config: get('.system__card--config'),
      }
    })
    expect(Math.abs(geometry.build.y - geometry.database.y)).toBeLessThanOrEqual(1)
    expect(Math.abs(geometry.database.y - geometry.retention.y)).toBeLessThanOrEqual(1)
    expect(Math.abs(geometry.workers.y - geometry.config.y)).toBeLessThanOrEqual(1)
    expect(geometry.workers.width).toBeLessThan(geometry.config.width)
  })

  test('system degradation is visible without exposing secret-bearing data', async ({ page }) => {
    await useScenario(page, 'database-degraded')
    await page.goto('/system')
    await expect(page.getByRole('heading', { name: '系统' })).toBeVisible()
    await expect(page.getByText('降级').first()).toBeVisible()
    await page.getByTestId('system-generate-diagnostics').click()
    const summary = page.getByTestId('system-diagnostics-alert')
    await expect(summary).toContainText('未包含任何密钥或 token')
    await expect(summary).not.toContainText('mock-pdv2.not-a-real-secret')
    await page.getByTestId('system-run-retention').click()
    await expect(page.getByTestId('retention-scope-preview')).toContainText('终态入站命令')
    await page.getByRole('button', { name: '取消' }).click()
    await expectNoSeriousAxe(page)
  })
})
