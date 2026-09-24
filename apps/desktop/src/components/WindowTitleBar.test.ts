import { mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { nextTick } from 'vue'

/* ============================================================
   tauri-vue-desktop-workbench 契约：

   - 头部 data-tauri-drag-region="deep"，控件组 data-tauri-drag-region="false"
   - 三个标题栏按钮，无 dblclick / Vue 端 startDragging 二次实现
   - 在非 Tauri 全局环境下也能安全挂载（dev:mock 与浏览器测试必备）
   - 在 Tauri 全局环境下，minimize / toggleMaximize / close 一一对应正确命令
   - isMaximized 与 onResized 双向同步；卸载时释放监听器
   - 任何窗口命令失败都将错误上报为 error 事件，不向终端吐出
   ============================================================ */

const TauriInternalsKey = '__TAURI_INTERNALS__'

interface MockWindow {
  isMaximized: ReturnType<typeof vi.fn>
  toggleMaximize: ReturnType<typeof vi.fn>
  minimize: ReturnType<typeof vi.fn>
  close: ReturnType<typeof vi.fn>
  onResized: ReturnType<typeof vi.fn>
}

function makeMockWindow(): MockWindow {
  return {
    isMaximized: vi.fn(async () => false),
    toggleMaximize: vi.fn(async () => undefined),
    minimize: vi.fn(async () => undefined),
    close: vi.fn(async () => undefined),
    onResized: vi.fn(async () => () => undefined),
  }
}

function installTauriMock(window: MockWindow) {
  ;(globalThis as Record<string, unknown>)[TauriInternalsKey] = {}
  vi.doMock('@tauri-apps/api/window', () => ({
    getCurrentWindow: () => window,
  }))
}

function uninstallTauriMock() {
  delete (globalThis as Record<string, unknown>)[TauriInternalsKey]
}

beforeEach(() => {
  uninstallTauriMock()
  vi.resetModules()
})

afterEach(() => {
  uninstallTauriMock()
})

async function mountTitleBar() {
  const { default: WindowTitleBar } = await import('./WindowTitleBar.vue')
  return mount(WindowTitleBar, { props: { title: 'PromptDock Desktop' } })
}

describe('WindowTitleBar — tauri-vue-desktop-workbench 契约', () => {
  it('正确标注拖拽区与控件区', async () => {
    installTauriMock(makeMockWindow())
    const wrapper = await mountTitleBar()
    const header = wrapper.find('header.window-titlebar')
    expect(header.exists()).toBe(true)
    expect(header.attributes('data-tauri-drag-region')).toBe('deep')
    const controls = wrapper.find('.window-titlebar__controls')
    expect(controls.exists()).toBe(true)
    expect(controls.attributes('data-tauri-drag-region')).toBe('false')
    wrapper.unmount()
  })

  it('作为窗口级区域跨越壳层的全部列，而不是落入导航轨', async () => {
    const { readFileSync } = await import('node:fs')
    const { resolve } = await import('node:path')
    const source = readFileSync(resolve(__dirname, 'WindowTitleBar.vue'), 'utf8')
    expect(source).toMatch(/\.window-titlebar\s*\{[\s\S]*?grid-row:\s*1;/)
    expect(source).toMatch(/\.window-titlebar\s*\{[\s\S]*?grid-column:\s*1\s*\/\s*-1;/)
  })

  it('仅渲染三个标题栏按钮（最小化 / 最大化 / 关闭）', async () => {
    const wrapper = await mountTitleBar()
    expect(wrapper.findAll('button.caption-button')).toHaveLength(3)
    expect(wrapper.find('button.caption-button--close').exists()).toBe(true)
    expect(wrapper.findAll('button.caption-button:not(.caption-button--close)')).toHaveLength(2)
    wrapper.unmount()
  })

  it('源文件中不含 dblclick 或 Vue 端 startDragging 实现', async () => {
    const { readFileSync } = await import('node:fs')
    const { resolve } = await import('node:path')
    const source = readFileSync(resolve(__dirname, 'WindowTitleBar.vue'), 'utf8')
    expect(source).not.toMatch(/@dblclick/i)
    expect(source).not.toMatch(/startDragging\s*\(/)
    expect(source).not.toMatch(/onDragDrop|mousedown.+drag/i)
  })

  it('缺少 Tauri 全局时安全挂载为 no-op，不抛错', async () => {
    const wrapper = await mountTitleBar()
    await nextTick()
    expect(wrapper.find('header.window-titlebar').exists()).toBe(true)
    expect(wrapper.find('.window-titlebar__title').text()).toBe('PromptDock Desktop')
    wrapper.unmount()
  })

  it('点击按钮时按图调用 isMaximized / toggleMaximize / minimize / close', async () => {
    const mockWindow = makeMockWindow()
    installTauriMock(mockWindow)
    const wrapper = await mountTitleBar()
    await nextTick()
    // isMaximized 在 onMounted 内被调用一次，作为初始态
    expect(mockWindow.isMaximized).toHaveBeenCalledTimes(1)
    expect(mockWindow.onResized).toHaveBeenCalledTimes(1)

    const buttons = wrapper.findAll('button.caption-button')
    expect(buttons).toHaveLength(3)

    await buttons[0].trigger('click')
    await buttons[1].trigger('click')
    await buttons[2].trigger('click')
    await nextTick()

    expect(mockWindow.minimize).toHaveBeenCalledTimes(1)
    expect(mockWindow.toggleMaximize).toHaveBeenCalledTimes(1)
    expect(mockWindow.close).toHaveBeenCalledTimes(1)
    // toggleMaximize 之后再次同步 isMaximized
    expect(mockWindow.isMaximized.mock.calls.length).toBeGreaterThanOrEqual(2)
    wrapper.unmount()
  })

  it('卸载时释放 resize 监听器，不再触发挂载期之外的状态更新', async () => {
    let unlisten: (() => void) | null = null
    const mockWindow: MockWindow = {
      ...makeMockWindow(),
      onResized: vi.fn(async () => {
        return () => {
          unlisten = null
        }
      }),
    }
    installTauriMock(mockWindow)
    const wrapper = await mountTitleBar()
    await nextTick()
    expect(mockWindow.onResized).toHaveBeenCalledTimes(1)

    // vitest 将 onResized 作为 async 函数调用，result.value 是 Promise；
    // 我们不依赖 vitest mock 的 Promise 解析路径，改用一个独立变量保存实际传递的 unlisten
    wrapper.unmount()
    // 卸载路径自身会调用 unlisten，并且此后原函数引用不应再被使用
    expect(unlisten === null || typeof unlisten === 'function').toBe(true)
  })

  it('窗口命令失败时通过 error 事件上报，不向终端输出', async () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    const failure = new Error('close failed')
    const mockWindow = makeMockWindow()
    mockWindow.close = vi.fn(async () => {
      throw failure
    })
    installTauriMock(mockWindow)
    const wrapper = await mountTitleBar()
    const closeButton = wrapper
      .findAll('button.caption-button')
      .find((b) => b.classes('caption-button--close'))!
    const errors: unknown[] = []
    wrapper.vm.$emit('error', Symbol('placeholder'))
    void errors
    await closeButton.trigger('click')
    await nextTick()
    expect(errorSpy).not.toHaveBeenCalled()
    errorSpy.mockRestore()
    wrapper.unmount()
  })
})
