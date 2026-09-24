<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted } from 'vue'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import WindowTitleBar from './components/WindowTitleBar.vue'
import AppNavigation from './components/AppNavigation.vue'
import NavIcon from './components/NavIcon.vue'
import StatusToast from './components/StatusToast.vue'
import DeliveriesView from './views/DeliveriesView.vue'
import DiagnosticsView from './views/DiagnosticsView.vue'
import IntegrationView from './views/IntegrationView.vue'
import NotificationsView from './views/NotificationsView.vue'
import OverviewView from './views/OverviewView.vue'
import { provideDesktopState } from './desktop/desktopState'
import { provideActivityController } from './desktop/activity'
import { provideNotificationHoldController } from './desktop/notificationHold'
import type { ViewKey } from './desktop/views'

const {
  status,
  launcher,
  activeView,
  busy,
  message,
  messageTone,
  loadFailed,
  currentMeta,
  hookLabel,
  hookTone,
  relayLabel,
  relayTone,
  navigate,
  refreshAll,
  initialize,
  dispose,
  clearMessage,
} = provideDesktopState()
const activity = provideActivityController()
const notificationHold = provideNotificationHoldController()

const views: Record<ViewKey, unknown> = {
  overview: OverviewView,
  integration: IntegrationView,
  notifications: NotificationsView,
  deliveries: DeliveriesView,
  diagnostics: DiagnosticsView,
}

const currentView = computed(() => views[activeView.value])

const dataDirectory = computed(() => status.value?.dataDirectory || '数据目录未初始化')

/* 标题栏显示固定产品名（窗口层面的「我在哪个应用」），不要随 view 改变；
   view 内的 h1 才是当前页面语义，已通过 sr-only 暴露给读屏。 */
const appTitle = 'PromptDock Desktop'

type NativeProbeWindow = Window & { __PROMPTDOCK_NATIVE_PROBE__?: unknown }
let disposed = false
const trayListeners: UnlistenFn[] = []

async function connectTray() {
  for (const registration of [
    listen('tray-activity-attention', () => {
      if (disposed) return
      navigate('overview')
      void activity.setFilter('attention')
    }),
    listen<unknown>('desktop-action-feedback', ({ payload }) => {
      if (disposed || !payload || typeof payload !== 'object') return
      const value = payload as Record<string, unknown>
      if (typeof value.message !== 'string' || !['success', 'danger'].includes(String(value.tone)))
        return
      message.value = value.message.slice(0, 500)
      messageTone.value = value.tone as 'success' | 'danger'
    }),
  ]) {
    try {
      const unlisten = await registration
      if (disposed) unlisten()
      else trayListeners.push(unlisten)
    } catch {
      /* ordinary browser previews do not own a native tray */
    }
  }
}

onMounted(() => {
  void connectTray()
  void (async () => {
    await initialize()
    await activity.start()
    await notificationHold.start()
    await nextTick()
    if ((window as NativeProbeWindow).__PROMPTDOCK_NATIVE_PROBE__ !== true) return

    const ready = Boolean(
      status.value &&
      launcher.value &&
      !loadFailed.value &&
      messageTone.value !== 'danger' &&
      !activity.error.value &&
      !notificationHold.error.value,
    )
    try {
      await emit('promptdock://native-startup-ready', { ready })
    } catch {
      // The self-test owns the timeout and treats a missing acknowledgement as failure.
    }
  })()
})

onUnmounted(() => {
  disposed = true
  trayListeners.splice(0).forEach((unlisten) => unlisten())
  activity.dispose()
  notificationHold.dispose()
  dispose()
})
</script>

<template>
  <div class="app-shell">
    <WindowTitleBar :title="appTitle">
      <template #icon>
        <!-- 标题栏左侧的品牌色块作为身份标识，紧扣项目唯一强调色 -->
        <span class="titlebar-mark" aria-hidden="true"></span>
      </template>
    </WindowTitleBar>

    <AppNavigation :active="activeView" @navigate="navigate" />

    <main
      class="workspace"
      aria-labelledby="view-title"
      aria-describedby="view-description"
      :aria-busy="busy"
    >
      <h1 id="view-title" class="sr-only">{{ currentMeta.title }}</h1>
      <p id="view-description" class="sr-only">{{ currentMeta.description }}</p>

      <StatusToast :tone="messageTone" :message="message" @dismiss="clearMessage" />

      <template v-if="status && launcher">
        <component :is="currentView" :key="activeView" />
      </template>

      <div v-else class="empty-state empty-state--loading" aria-live="polite">
        <NavIcon v-if="loadFailed" name="diagnostics" :size="24" />
        <span v-else class="loading-indicator" aria-hidden="true"></span>
        <strong class="empty-state__title">{{
          loadFailed ? '本机状态不可用' : '正在读取本机状态'
        }}</strong>
        <span class="empty-state__description">{{
          loadFailed
            ? '请在 PromptDock Desktop 应用内使用设置；也可重新读取本机状态。'
            : '正在连接桌面运行时。'
        }}</span>
        <button v-if="loadFailed" class="button button--primary" type="button" @click="refreshAll">
          重新读取本机状态
        </button>
      </div>
    </main>

    <footer class="global-status" aria-label="全局状态">
      <button type="button" @click="navigate('integration')">
        <span :class="`status-dot status-dot--${hookTone}`" aria-hidden="true"></span>Hook：{{
          hookLabel
        }}
      </button>
      <button type="button" @click="navigate('notifications')">
        <span :class="`status-dot status-dot--${relayTone}`" aria-hidden="true"></span>Relay：{{
          relayLabel
        }}
      </button>
      <button type="button" @click="navigate('diagnostics')">
        <span class="status-dot status-dot--neutral" aria-hidden="true"></span>后台：{{
          status ? '应用已启动' : '状态未知'
        }}
      </button>
      <span class="global-status__path mono truncate" :title="dataDirectory">{{
        dataDirectory
      }}</span>
      <!-- 刷新动作随标题栏一起出现会变成冗余；保留在状态栏维持「一个刷新动作」 -->
      <button
        class="global-status__refresh"
        type="button"
        :disabled="busy"
        :aria-busy="busy"
        @click="refreshAll"
      >
        <NavIcon name="refresh" :size="12" />
        刷新状态
      </button>
    </footer>
  </div>
</template>

<style scoped>
.app-shell {
  display: grid;
  width: 100%;
  height: 100%;
  grid-template-rows: var(--titlebar-height) minmax(0, 1fr) auto;
  grid-template-columns: var(--rail-width) minmax(0, 1fr);
  overflow: hidden;
  background: var(--bg-window);
  color: var(--text-primary);
}

.workspace {
  min-width: 0;
  min-height: 0;
  overflow: auto;
  padding: var(--workspace-padding);
  background: var(--bg-window);
}

.global-status {
  display: flex;
  min-width: 0;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--space-1);
  padding: 0 var(--statusbar-padding-inline);
  border-top: 1px solid var(--border-default);
  background: var(--bg-canvas);
  color: var(--text-muted);
  font-family: var(--font-mono);
  font-size: var(--font-size-meta);
}

.global-status button {
  display: inline-flex;
  min-height: var(--statusbar-control-height);
  align-items: center;
  gap: var(--space-2);
  padding: 0 var(--space-2);
  border: 0;
  border-inline-end: 1px solid var(--border-default);
  background: transparent;
  color: inherit;
  font: inherit;
  cursor: pointer;
  transition: color var(--duration-hover) var(--ease-out);
}

.global-status button:hover {
  color: var(--text-primary);
}

.global-status__path {
  min-width: 0;
  margin-inline-start: auto;
}

.global-status__refresh {
  flex: none;
  margin-inline-start: var(--space-2);
}

.global-status button:last-child {
  border-inline-end: 0;
}

.status-dot {
  width: var(--status-dot-size);
  height: var(--status-dot-size);
  border-radius: 50%;
  background: var(--text-muted);
}

.status-dot--success {
  background: var(--status-success);
}

.status-dot--warning {
  background: var(--status-warning);
}

.status-dot--danger {
  background: var(--status-danger);
}

/* 标题栏左侧品牌色块：与 --brand-primary 同色，方形紧贴 35px 高图标槽
   视觉权重与一行正文标签相当，使 title 不被抢戏 */
.titlebar-mark {
  display: block;
  width: var(--titlebar-mark-size);
  height: var(--titlebar-mark-size);
  border-radius: var(--titlebar-mark-radius);
  background: var(--brand-primary);
  box-shadow: 0 0 0 1px var(--titlebar-mark-ring);
}

/* 断点：CSS 媒体查询无法插值自定义属性，此处为唯一声明点 */
@media (max-width: 720px) {
  .app-shell {
    grid-template-columns: var(--rail-width-compact) minmax(0, 1fr);
  }

  .global-status__path {
    display: none;
  }

  .workspace {
    padding: var(--workspace-padding-compact);
  }
}
</style>
