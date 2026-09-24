import { test, expect } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

test.describe('A0 scaffold', () => {
  test('redirects root to overview', async ({ page }) => {
    await page.goto('/')
    await expect(page).toHaveURL(/\/overview$/)
  })

  test('topbar exposes the route title and Mock environment without redundant chrome', async ({
    page,
  }) => {
    await page.goto('/overview')
    await expect(page.locator('#app-route-title')).toHaveText('总览')
    await expect(page.locator('h1')).toHaveCount(1)
    await expect(
      page.locator('.topbar__mark, .topbar__breadcrumb, .topbar__session, .topbar__badge-dot'),
    ).toHaveCount(0)
    await expect(page.locator('main')).toHaveAttribute('aria-labelledby', 'app-route-title')
    const badge = page.getByTestId('env-badge-mock')
    await expect(badge).toBeVisible()
    await expect(badge).toHaveText('Mock')
  })

  test('shell fills the viewport while main owns scrolling', async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.goto('/overview')
    const geometry = await page.evaluate(() => {
      const sidebar = document.querySelector<HTMLElement>('.app-shell__sidebar')
      const workspace = document.querySelector<HTMLElement>('[data-testid="app-workspace"]')
      const main = document.querySelector<HTMLElement>('main')
      if (!sidebar || !workspace || !main) throw new Error('shell landmarks missing')
      return {
        sidebarTop: sidebar.getBoundingClientRect().top,
        sidebarHeight: sidebar.getBoundingClientRect().height,
        workspaceTop: workspace.getBoundingClientRect().top,
        workspaceHeight: workspace.getBoundingClientRect().height,
        bodyOverflow: document.body.scrollHeight > window.innerHeight,
        mainOverflow: main.scrollHeight >= main.clientHeight,
      }
    })
    expect(Math.abs(geometry.sidebarTop)).toBeLessThanOrEqual(1)
    expect(Math.abs(geometry.sidebarHeight - 900)).toBeLessThanOrEqual(1)
    expect(Math.abs(geometry.workspaceTop)).toBeLessThanOrEqual(1)
    expect(Math.abs(geometry.workspaceHeight - 900)).toBeLessThanOrEqual(1)
    expect(geometry.bodyOverflow).toBe(false)
    expect(geometry.mainOverflow).toBe(true)
  })

  test('sidebar exposes five navigation links', async ({ page }) => {
    await page.goto('/overview')
    const links = page.locator('nav.sidebar a.sidebar__link')
    await expect(links).toHaveCount(5)
    await expect(links.nth(0)).toContainText('总览')
    await expect(links.nth(4)).toContainText('系统')
  })

  test('navigates to devices page', async ({ page }) => {
    await page.goto('/overview')
    await page.getByRole('link', { name: '设备' }).click()
    await expect(page).toHaveURL(/\/devices$/)
    await expect(page.getByRole('heading', { name: '设备' })).toBeVisible()
  })

  test('renders the five empty pages', async ({ page }) => {
    const paths = ['/overview', '/devices', '/wechat', '/queue', '/system']
    for (const path of paths) {
      await page.goto(path)
      await expect(page.locator('main')).toBeVisible()
    }
  })

  test('unknown path shows 404', async ({ page }) => {
    const response = await page.goto('/this-does-not-exist')
    expect(response?.status()).toBe(200)
    await expect(page.getByRole('heading', { name: '页面不存在' })).toBeVisible()
  })

  test('overview page passes basic axe scan', async ({ page }) => {
    await page.goto('/overview')
    const results = await new AxeBuilder({ page }).analyze()
    const serious = results.violations.filter(
      (v) => v.impact === 'serious' || v.impact === 'critical',
    )
    expect(serious).toEqual([])
  })
})
