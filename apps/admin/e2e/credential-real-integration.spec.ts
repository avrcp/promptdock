import { expect, test } from '@playwright/test'

// The fixture contains a deterministic sentinel token only. Keep this test
// trace-free so no credential-shaped DOM data is retained in artifacts.
test.use({ trace: 'off' })

test('removes the credential sentinel from production DOM immediately after close', async ({
  page,
}) => {
  await page.goto('/devices')
  await page.getByTestId('device-create').click()
  await page.getByTestId('device-create-name').fill('credential-lifecycle-check')
  await page.getByRole('button', { name: '创建并显示 Token' }).click()

  const token = page.getByTestId('credential-receipt-token')
  await expect(token).toBeVisible()
  const sentinel = await token.inputValue()
  expect(sentinel).not.toBe('')

  await page.getByRole('button', { name: '我已保存，关闭' }).click()
  await expect(page.getByTestId('credential-receipt')).toHaveCount(0)
  expect(
    await page.locator('body').evaluate((body, value) => {
      return !body.textContent?.includes(value) && !body.innerHTML.includes(value)
    }, sentinel),
  ).toBe(true)

  await page.getByTestId('device-create').click()
  await page.getByTestId('device-create-name').fill('credential-route-leave-check')
  await page.getByRole('button', { name: '创建并显示 Token' }).click()
  const routeSentinel = await page.getByTestId('credential-receipt-token').inputValue()
  // A modal correctly blocks pointer interaction with the sidebar. Navigate
  // through the router boundary directly to verify unmount cleanup.
  await page.goto('/overview')
  await expect(page.getByRole('heading', { name: '总览' })).toBeVisible()
  await expect(page.getByTestId('credential-receipt')).toHaveCount(0)
  expect(
    await page.locator('body').evaluate((body, value) => {
      return !body.textContent?.includes(value) && !body.innerHTML.includes(value)
    }, routeSentinel),
  ).toBe(true)
})
