import { test, expect } from '@playwright/test'

test.describe('dev scenario selector', () => {
  test('renders in dev build only', async ({ page }) => {
    await page.goto('/overview')
    const selector = page.getByTestId('dev-scenario-select')
    await expect(selector).toBeVisible()
    const options = await selector.locator('option').allTextContents()
    expect(options.length).toBe(12)
  })

  test('switches scenarios and persists in session storage', async ({ page }) => {
    await page.goto('/overview')
    const selector = page.getByTestId('dev-scenario-select')
    await selector.selectOption('queue-backlog')
    const stored = await page.evaluate(() =>
      window.sessionStorage.getItem('promptdock-relay-admin:dev-scenario'),
    )
    expect(stored).toBe('queue-backlog')
  })

  test('switching to a scenario updates the active option in the selector', async ({ page }) => {
    await page.goto('/overview')
    const selector = page.getByTestId('dev-scenario-select')
    await selector.selectOption('relay-unavailable')
    await expect(selector).toHaveValue('relay-unavailable')
  })
})
