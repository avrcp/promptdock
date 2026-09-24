import { expect, test } from '@playwright/test'

test.describe('Relay Admin v2 production integration', () => {
  test('uses the fixture-backed HTTP repository and exposes Admin-owned login', async ({
    page,
  }) => {
    await page.goto('/overview')
    await expect(page.getByTestId('env-badge-production')).toBeVisible()
    await expect(page.getByTestId('dev-scenario-select')).toHaveCount(0)
    await expect(page.getByTestId('stat-relay')).toContainText('正常')
    await expect(page.getByTestId('stat-relay')).toContainText('0.6.0-rc.1')

    await page.goto('/devices')
    await expect(page.getByTestId('device-create')).toBeEnabled()
    await expect(page.getByTestId('device-details').first()).toBeVisible()
    await expect(page.locator('[aria-label="模拟数据"]')).toHaveCount(0)

    await page.goto('/wechat')
    await expect(page.getByTestId('wechat-login')).toBeEnabled()
    await expect(page.getByText('云端微信通道仅由 Relay Admin 管理')).toBeVisible()
    await expect(page.getByTestId('wechat-test')).toBeEnabled()
    await expect(page.getByTestId('wechat-disconnect')).toBeEnabled()

    await page.goto('/system')
    await expect(page.getByTestId('system-run-retention')).toBeEnabled()
  })

  test('starts directly, renders only SVG QR data, polls, verifies and cancels', async ({
    page,
  }) => {
    await page.goto('/wechat')
    const login = page.getByTestId('wechat-login')
    await expect(login).toBeEnabled()
    await login.click()
    await expect(page.getByTestId('wechat-qr')).toBeVisible()
    await expect(page.locator('[data-testid="wechat-qr"] svg')).toBeVisible()
    await expect(page.locator('body')).not.toContainText('contract-admin-qr-content')

    await expect(page.getByTestId('wechat-login-verify-code')).toBeVisible({ timeout: 5_000 })
    const code = page.getByTestId('wechat-login-verify-code')
    await code.fill('123456')
    await page.getByRole('button', { name: '提交验证码' }).click()
    await expect(code).toHaveValue('')

    await page.getByRole('button', { name: '取消' }).click()
    await expect(page.getByTestId('wechat-qr')).toHaveCount(0)
  })
})
