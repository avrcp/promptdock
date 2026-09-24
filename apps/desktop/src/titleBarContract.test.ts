import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

/* ============================================================
   tauri-vue-desktop-workbench host 配置契约：

   - main 窗口必须 decorations: false，否则会出现「原生 + 自定义」双标题栏
   - backgroundColor 必须与 tokens.css 的 --bg-canvas 解析值一致，
     否则冷启动会闪一帧与标题栏不一致的背景
   - capability 必须只显式声明 caption 标题栏用到的 8 个窗口权限，
     禁止 core:default / core:event:default / core:window:default 兜底
   ============================================================ */

const projectRoot = resolve(__dirname, '..')
const config = JSON.parse(
  readFileSync(resolve(projectRoot, 'src-tauri/tauri.conf.json'), 'utf8'),
) as {
  app: {
    windows: Array<{
      label: string
      decorations: boolean
      visible?: boolean
      backgroundColor?: string
    }>
  }
}
const capability = JSON.parse(
  readFileSync(resolve(projectRoot, 'src-tauri/capabilities/main.json'), 'utf8'),
) as { windows: string[]; permissions: string[] }

const requiredWindowPermissions = [
  'core:event:allow-listen',
  'core:event:allow-unlisten',
  'core:window:allow-is-maximized',
  'core:window:allow-internal-toggle-maximize',
  'core:window:allow-close',
  'core:window:allow-minimize',
  'core:window:allow-start-dragging',
  'core:window:allow-toggle-maximize',
]

function resolveCanvasToken(tokens: string): string {
  const canvasMatch = tokens.match(/--bg-canvas:\s*([^;]+);/)
  if (!canvasMatch) throw new Error('--bg-canvas 未在 tokens.css 定义')
  const canvasValue = canvasMatch[1]!.trim()
  if (canvasValue.startsWith('var(')) {
    const raw = canvasValue.slice(4, -1).trim()
    const tokenName = raw.startsWith('--') ? raw.slice(2) : raw
    const direct = new RegExp(`--${tokenName}:\\s*([^;]+);`).exec(tokens)
    if (!direct) throw new Error(`${canvasValue} 解析失败`)
    return direct[1]!.trim()
  }
  return canvasValue
}

describe('tauri-vue-desktop-workbench 配置契约', () => {
  it('主窗口存在且装饰关闭，避免出现双标题栏', () => {
    expect(config.app.windows).toHaveLength(1)
    const window = config.app.windows[0]!
    expect(window.label).toBe('main')
    expect(window.decorations).toBe(false)
  })

  it('主窗口 backgroundColor 与 tokens.css --bg-canvas 解析值一致', () => {
    const tokens = readFileSync(
      resolve(projectRoot, 'src/design-system/tokens.css'),
      'utf8',
    )
    const expected = resolveCanvasToken(tokens)
    expect(config.app.windows[0]!.backgroundColor?.toLowerCase()).toBe(expected.toLowerCase())
  })

  it('capability 显式声明了 8 个标题栏窗口权限，未启用任何兜底 default', () => {
    const permissionSet = new Set(capability.permissions)
    for (const required of requiredWindowPermissions) {
      expect(permissionSet.has(required)).toBe(true)
    }
    expect(
      capability.permissions.some((p) => p === 'core:default' || p.endsWith(':default')),
    ).toBe(false)
    expect(capability.windows).toEqual(['main'])
  })
})
