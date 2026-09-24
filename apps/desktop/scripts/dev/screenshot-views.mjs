#!/usr/bin/env node
/**
 * 设计走查截图：5 个视图 × 3 种宽度（默认、最小、200% 缩放等效视口）。
 *
 * 用法：先起 mock dev server，再运行本脚本。
 *   pnpm dev:mock
 *   node scripts/dev/screenshot-views.mjs [输出目录]
 *
 * 需要 playwright-core（devDependency）和 Playwright 的 Chromium。运行
 * `pnpm exec playwright install chromium` 一次性下载，本脚本不再回退到任何
 * 私有工作区路径。
 */

import { mkdirSync } from 'node:fs'
import { createRequire } from 'node:module'
import { join, resolve } from 'node:path'

const localRequire = createRequire(new URL('../../package.json', import.meta.url))
let chromium
try {
  chromium = localRequire('playwright-core').chromium
} catch {
  throw new Error('找不到 playwright-core：请先执行 `pnpm install` 安装 workspace 依赖。')
}

const OUT_DIR = resolve(process.argv[2] ?? '.design-review')
const BASE_URL = process.env.PD_MOCK_URL ?? 'http://localhost:5199'

const EXECUTABLE = process.env.PD_CHROMIUM_PATH ?? join(
  process.env.LOCALAPPDATA ?? '',
  'ms-playwright/chromium-1243/chrome-win64/chrome.exe',
)

const views = [
  { key: 'overview', label: '总览', first: true },
  { key: 'integration', label: '集成（桌面与 Hook）' },
  { key: 'notifications', label: '通知（Relay 与通知）' },
  { key: 'deliveries', label: '投递（投递记录）' },
  { key: 'diagnostics', label: '诊断（后台与诊断）' },
]

const sizes = [
  { name: 'default', width: 1180, height: 760 },
  { name: 'narrow', width: 720, height: 560 },
  { name: 'zoom200', width: 360, height: 280 },
]

mkdirSync(OUT_DIR, { recursive: true })

const browser = await chromium.launch({ executablePath: EXECUTABLE })

try {
  for (const size of sizes) {
    const context = await browser.newContext({
      viewport: { width: size.width, height: size.height },
      deviceScaleFactor: 2, // 视网膜密度，走查文字与焦点环更接近真实观感
    })
    const page = await context.newPage()
    await page.goto(BASE_URL, { waitUntil: 'networkidle' })
    await page.waitForSelector('.workspace .panel', { timeout: 10_000 })

    const overflow = await page.evaluate(() => {
      const shell = document.querySelector('.app-shell')
      const footer = document.querySelector('.global-status')
      if (!(shell instanceof HTMLElement) || !(footer instanceof HTMLElement)) {
        throw new Error('未找到应用壳层或全局状态栏')
      }
      const clippedButton = Array.from(footer.querySelectorAll('button')).find(
        (button) => button.scrollHeight > button.clientHeight,
      )
      return {
        shell: shell.scrollHeight > shell.clientHeight,
        footer: footer.scrollHeight > footer.clientHeight,
        button: Boolean(clippedButton),
      }
    })
    if (overflow.shell || overflow.footer || overflow.button) {
      throw new Error(`状态栏在 ${size.name} 视口发生裁切: ${JSON.stringify(overflow)}`)
    }

    for (const view of views) {
      if (!view.first) {
        await page.click(`button[aria-label="${view.label}"]`)
        await page.waitForTimeout(250) // 视图入场动画 160ms
      }
      const file = join(OUT_DIR, `${view.key}-${size.name}.png`)
      await page.screenshot({ path: file })
      console.log('saved', file)
    }

    // 额外两张：键盘焦点可见性（Tab 到导航轨）与错误横幅
    await page.click('button[aria-label="总览"]')
    await page.waitForTimeout(250)
    await page.keyboard.press('Tab')
    await page.keyboard.press('Tab')
    await page.screenshot({ path: join(OUT_DIR, `focus-${size.name}.png`) })
    console.log('saved', join(OUT_DIR, `focus-${size.name}.png`))

    // 卸载确认对话框（破坏性动作的确认流程）
    await page.click('button[aria-label="集成（桌面与 Hook）"]')
    await page.waitForTimeout(250)
    await page.click('button:has-text("卸载")')
    await page.waitForSelector('[role="alertdialog"]', { timeout: 5_000 })
    await page.screenshot({ path: join(OUT_DIR, `confirm-dialog-${size.name}.png`) })
    console.log('saved', join(OUT_DIR, `confirm-dialog-${size.name}.png`))
    await page.keyboard.press('Escape')
    await page.waitForTimeout(120)

    await context.close()
  }
} finally {
  await browser.close()
}
console.log('\ndone:', OUT_DIR)
