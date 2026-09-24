<script setup lang="ts">
import { getCurrentWindow, type Window } from '@tauri-apps/api/window'
import { onBeforeUnmount, onMounted, ref } from 'vue'

defineProps<{
  /** 显示在标题栏中部的内容，建议短文本（单语/单标识符），长字符串会被省略 */
  title: string
  /** 可选的应用图标 URL（PNG / SVG dataURL）。未提供时渲染 #icon 具名插槽 */
  iconUrl?: string
}>()

/* ============================================================
   生命周期与原生命令 - 与 tauri-vue-desktop-workbench 标题栏契约对齐：

   - getCurrentWindow() 同步缓存，避免 caption 点击与用户手势被无关 await 切断。
   - isMaximized 通过 onResized 订阅，再调用 toggleMaximize 后同步刷新，
     不在控件处另写 dblclick 处理（会与 Tauri 的内部双击命令竞争导致二次切换）。
   - 全程只暴露 'error' 事件，不在 Vue 端与 Rust 的 CloseRequested 抢同一个关闭路径。
   ============================================================ */
const emit = defineEmits<{ error: [error: unknown] }>()

const isMaximized = ref(false)
let appWindow: Window | null = null
let unlistenResize: (() => void) | undefined
let syncSequence = 0
let mounted = false

function reportError(error: unknown) {
  if (mounted) emit('error', error)
}

function windowHandle(): Window | null {
  if (appWindow) return appWindow
  if (!('__TAURI_INTERNALS__' in globalThis)) return null
  try {
    appWindow = getCurrentWindow()
    return appWindow
  } catch (error) {
    reportError(error)
    return null
  }
}

function releaseResizeListener(unlisten: (() => void) | undefined) {
  if (!unlisten) return
  try {
    unlisten()
  } catch (error) {
    reportError(error)
  }
}

async function syncMaximized() {
  if (!mounted) return
  const window = windowHandle()
  if (!window) return
  const sequence = ++syncSequence
  try {
    const next = await window.isMaximized()
    if (mounted && sequence === syncSequence) isMaximized.value = next
  } catch (error) {
    if (sequence === syncSequence) reportError(error)
  }
}

async function toggleMaximize() {
  const window = windowHandle()
  if (!window) return
  try {
    await window.toggleMaximize()
    await syncMaximized()
  } catch (error) {
    reportError(error)
  }
}

async function runWindowCommand(command: 'minimize' | 'close') {
  const window = windowHandle()
  if (!window) return
  try {
    await window[command]()
  } catch (error) {
    reportError(error)
  }
}

onMounted(async () => {
  mounted = true
  const window = windowHandle()
  if (!window) return
  await syncMaximized()
  if (!mounted) return
  try {
    const unlisten = await window.onResized(() => void syncMaximized())
    if (mounted) unlistenResize = unlisten
    else releaseResizeListener(unlisten)
  } catch (error) {
    reportError(error)
  }
})

onBeforeUnmount(() => {
  mounted = false
  syncSequence += 1
  const unlisten = unlistenResize
  unlistenResize = undefined
  releaseResizeListener(unlisten)
})
</script>

<template>
  <header class="window-titlebar" role="banner" data-tauri-drag-region="deep">
    <div class="window-titlebar__identity">
      <span class="window-titlebar__icon-slot">
        <img v-if="iconUrl" class="window-titlebar__icon" :src="iconUrl" alt="" />
        <slot v-else name="icon" />
      </span>
      <span class="window-titlebar__title">{{ title }}</span>
    </div>
    <div class="window-titlebar__drag-space" aria-hidden="true" />
    <div
      class="window-titlebar__controls"
      role="group"
      aria-label="窗口控制"
      data-tauri-drag-region="false"
    >
      <button
        type="button"
        class="caption-button"
        title="最小化"
        aria-label="最小化窗口"
        @click="runWindowCommand('minimize')"
      >
        <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
          <path d="M2 6h8" />
        </svg>
      </button>
      <button
        type="button"
        class="caption-button"
        :title="isMaximized ? '还原' : '最大化'"
        :aria-label="isMaximized ? '还原窗口' : '最大化窗口'"
        @click="toggleMaximize"
      >
        <svg v-if="!isMaximized" width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
          <rect x="2.5" y="2.5" width="7" height="7" rx="1" />
        </svg>
        <svg v-else width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
          <rect x="3.5" y="1.5" width="7" height="7" rx="1" />
          <path d="M1.5 4.5v4a1 1 0 0 0 1 1h4" />
        </svg>
      </button>
      <button
        type="button"
        class="caption-button caption-button--close"
        title="关闭"
        aria-label="关闭窗口"
        @click="runWindowCommand('close')"
      >
        <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
          <path d="m2.5 2.5 7 7M9.5 2.5l-7 7" />
        </svg>
      </button>
    </div>
  </header>
</template>

<style scoped>
.window-titlebar {
  /* App shell has a navigation rail plus workspace columns.  The chrome is a
     window region, not rail content, so it must own the complete first row. */
  grid-row: 1;
  grid-column: 1 / -1;
  display: flex;
  min-width: 0;
  height: var(--titlebar-height);
  align-items: stretch;
  border-bottom: 1px solid var(--border-default);
  background: var(--bg-canvas);
  color: var(--text-secondary);
  /* 拖拽区必须强制禁止选中和聚焦样式，否则一次划选会触发原生行为 */
  user-select: none;
  -webkit-user-select: none;
}

.window-titlebar__identity {
  display: flex;
  min-width: 0;
  height: 100%;
  align-items: center;
}

.window-titlebar__icon-slot {
  display: grid;
  width: var(--titlebar-height);
  height: 100%;
  flex: 0 0 var(--titlebar-height);
  place-items: center;
}

.window-titlebar__icon {
  width: var(--titlebar-icon-size);
  height: var(--titlebar-icon-size);
}

.window-titlebar__title {
  min-width: 0;
  padding-inline-end: var(--space-3);
  overflow: hidden;
  font-family: var(--font);
  font-size: var(--font-size-label);
  font-weight: var(--weight-medium);
  line-height: 1;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.window-titlebar__drag-space {
  min-width: var(--titlebar-drag-min);
  flex: 1;
  align-self: stretch;
}

.window-titlebar__controls {
  display: flex;
  width: var(--caption-controls-width);
  height: 100%;
  flex: 0 0 var(--caption-controls-width);
}

.caption-button {
  display: grid;
  width: var(--caption-button-width);
  height: 100%;
  padding: 0;
  place-items: center;
  border: 0;
  border-radius: 0;
  background: transparent;
  color: var(--text-secondary);
  cursor: default;
  transition:
    background-color var(--duration-hover) var(--ease-out),
    color var(--duration-hover) var(--ease-out);
}

.caption-button:hover {
  background: var(--caption-hover);
  color: var(--text-primary);
}

.caption-button:active {
  background: var(--caption-pressed);
  color: var(--text-primary);
}

.caption-button--close:hover {
  background: var(--caption-close-hover);
  color: var(--caption-close-glyph);
}

.caption-button--close:active {
  background: var(--caption-close-pressed);
  color: var(--caption-close-glyph);
}

.caption-button svg {
  fill: none;
  stroke: var(--caption-glyph-stroke);
  stroke-linecap: round;
  stroke-width: var(--caption-glyph-stroke-width);
}

.caption-button:focus-visible {
  outline: var(--focus-width) solid var(--focus-ring);
  outline-offset: calc(-1 * var(--focus-width));
}

@media (prefers-reduced-motion: reduce) {
  .caption-button {
    transition: none;
  }
}
</style>
