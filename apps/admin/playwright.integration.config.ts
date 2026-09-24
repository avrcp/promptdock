import { defineConfig, devices } from '@playwright/test'

const port = Number(process.env.ADMIN_INTEGRATION_PORT ?? 4174)

export default defineConfig({
  testDir: './e2e',
  testMatch: ['**/real-integration.spec.ts', '**/credential-real-integration.spec.ts'],
  fullyParallel: false,
  reporter: [['list'], ['html', { open: 'never' }]],
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'node scripts/admin-fixture-server.mjs',
    url: `http://127.0.0.1:${port}`,
    reuseExistingServer: false,
    timeout: 60_000,
  },
})
