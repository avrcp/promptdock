<script setup lang="ts">
import { computed, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import DeliveryTable from '../components/DeliveryTable.vue'
import HookHealthPanel from '../components/HookHealthPanel.vue'
import EmptyState from '../components/EmptyState.vue'
import NavIcon from '../components/NavIcon.vue'
import PanelSection from '../components/PanelSection.vue'
import StatusChip from '../components/StatusChip.vue'
import ActivityCard from '../components/ActivityCard.vue'
import { activityFilters, useActivityController, type ActivityFilter } from '../desktop/activity'
import { useDesktopState } from '../desktop/desktopState'

const {
  status,
  launcher,
  relay,
  deliveries,
  autostart,
  busy,
  hookLabel,
  hookTone,
  hostLabel,
  hostTone,
  relayLabel,
  relayTone,
  navigate,
  act,
} = useDesktopState()

const activity = useActivityController()
const {
  filter: activityFilter,
  items: activityItems,
  counts: activityCounts,
  nextCursor: activityNextCursor,
  pagePending: activityPagePending,
  detailPending: activityDetailPending,
  actionPending: activityActionPending,
  error: activityError,
  selected: selectedActivity,
  capped: activityCapped,
  refresh: refreshActivity,
  loadMore: loadMoreActivity,
  setFilter: setActivityFilter,
  openDetail: openActivityDetail,
  closeDetail: closeActivityDetail,
  acknowledge: acknowledgeActivity,
  markSeen: markActivitySeen,
} = activity
const activityFilterLabels: Record<ActivityFilter, string> = {
  attention: '需要处理',
  started: '已开始',
  results: '结果',
  deliveryIssues: '投递问题',
  recent: '最近',
}
const focusBusy = ref(false)
const focusFeedback = ref('')
async function focusDesktop() {
  if (focusBusy.value) return
  focusBusy.value = true
  focusFeedback.value = ''
  try {
    const receipt = await invoke<unknown>('desktop_host_focus')
    const status =
      typeof receipt === 'object' && receipt !== null && 'status' in receipt
        ? (receipt as { status?: unknown }).status
        : null
    focusFeedback.value =
      {
        focused: '已切回桌面应用。',
        host_not_running: '桌面应用未运行，请先启动。',
        window_not_found: '已检测到桌面应用，但未找到可激活窗口。',
        foreground_denied: 'Windows 未允许切换到该窗口，请手动从任务栏打开。',
        multiple_candidates: '检测到多个桌面应用窗口，请手动选择要继续的窗口。',
      }[typeof status === 'string' ? status : ''] ?? '切换桌面应用的回执无法确认。'
  } catch {
    focusFeedback.value = '暂时无法切回桌面应用，请从任务栏打开。'
  } finally {
    focusBusy.value = false
  }
}

const hostDetail = computed(
  () => launcher.value?.config.desktop.selectedExecutable || '尚未选择可执行文件',
)
const relayDetail = computed(() => relay.value?.baseUrl || '尚未配置服务器地址')
const hookDetail = computed(() => (status.value?.hookHome ? '已指定配置目录' : '尚未指定配置目录'))
const backgroundDetail = computed(() => (autostart.value ? '已启用开机自启' : '未启用开机自启'))

const pathSteps = computed(() => [
  {
    title: 'Codex 会话',
    detail: launcher.value?.selectedRunning ? '宿主正在运行' : '等待宿主运行',
  },
  { title: 'Hook', detail: hookLabel.value },
  { title: 'PromptDock', detail: '本地通知应用已启动' },
  { title: 'Relay', detail: relayLabel.value },
  { title: '微信', detail: '手机显示需人工核验' },
])
</script>

<template>
  <div class="view view--overview">
    <PanelSection
      class="activity-panel"
      title="会话活动"
      description="只显示本机安全元数据。已开始表示已观察到开始；结果页和微信接收分别以投递记录为准。"
    >
      <template #actions>
        <button
          class="button button--secondary"
          type="button"
          :disabled="activityPagePending"
          @click="refreshActivity()"
        >
          <NavIcon name="refresh" />{{ activityPagePending ? '正在刷新…' : '刷新活动' }}
        </button>
      </template>
      <div class="activity-filters" role="group" aria-label="活动过滤">
        <button
          v-for="item in activityFilters"
          :key="item"
          class="button button--secondary activity-filter"
          :class="{ 'activity-filter--active': activityFilter === item }"
          type="button"
          :aria-pressed="activityFilter === item"
          @click="setActivityFilter(item)"
        >
          {{ activityFilterLabels[item] }}<span class="numeric">{{ activityCounts[item] }}</span>
        </button>
      </div>
      <p v-if="activityError" class="field__hint" role="status">{{ activityError }}</p>
      <div v-if="activityItems.length" class="activity-list">
        <ActivityCard
          v-for="item in activityItems"
          :key="item.runKey"
          :item="item"
          :detail="selectedActivity"
          :detail-pending="activityDetailPending"
          :action-pending="activityActionPending"
          @select="openActivityDetail"
          @close="closeActivityDetail"
          @acknowledge="acknowledgeActivity"
          @mark-seen="markActivitySeen"
          @open-deliveries="navigate('deliveries')"
        />
      </div>
      <EmptyState
        v-else-if="!activityPagePending"
        icon="diagnostics"
        title="当前没有此类活动"
        description="活动出现后会在这里显示安全状态与时间线，不会显示原始任务内容。"
      />
      <div v-if="activityItems.length" class="panel__actions activity-more">
        <p v-if="activityCapped" class="field__hint">
          已达到本地活动缓存上限；刷新可重新从最近活动查看。
        </p>
        <button
          v-else-if="activityNextCursor"
          class="button button--secondary"
          type="button"
          :disabled="activityPagePending"
          @click="loadMoreActivity()"
        >
          {{ activityPagePending ? '正在读取…' : '加载更多' }}
        </button>
        <p v-else class="field__hint">已到最早一条。</p>
      </div>
    </PanelSection>

    <HookHealthPanel compact />
    <ul class="status-grid" aria-label="当前状态">
      <li>
        <button class="status-card" type="button" @click="navigate('integration')">
          <NavIcon name="launch" />
          <span class="status-card__text">
            <strong>桌面宿主</strong>
            <small class="truncate" :title="hostDetail">{{ hostDetail }}</small>
          </span>
          <StatusChip :tone="hostTone" :label="hostLabel" />
        </button>
      </li>
      <li>
        <button class="status-card" type="button" @click="navigate('integration')">
          <NavIcon name="hook" />
          <span class="status-card__text">
            <strong>Codex Hook</strong>
            <small class="truncate">{{ hookDetail }}</small>
          </span>
          <StatusChip :tone="hookTone" :label="hookLabel" />
        </button>
      </li>
      <li>
        <button class="status-card" type="button" @click="navigate('notifications')">
          <NavIcon name="relay" />
          <span class="status-card__text">
            <strong>Relay</strong>
            <small class="truncate" :title="relayDetail">{{ relayDetail }}</small>
          </span>
          <StatusChip :tone="relayTone" :label="relayLabel" />
        </button>
      </li>
      <li>
        <button class="status-card" type="button" @click="navigate('diagnostics')">
          <NavIcon name="background" />
          <span class="status-card__text">
            <strong>后台</strong>
            <small class="truncate">{{ backgroundDetail }}</small>
          </span>
          <StatusChip tone="neutral" label="应用已启动" />
        </button>
      </li>
    </ul>

    <div class="overview-grid">
      <PanelSection class="quick-start" title="快速启动">
        <template #actions>
          <StatusChip :tone="hostTone" :label="hostLabel" />
        </template>
        <dl class="summary-list">
          <div>
            <dt>已选宿主</dt>
            <dd class="mono">{{ hostDetail }}</dd>
          </div>
          <div>
            <dt>启动代理</dt>
            <dd>{{ launcher?.config.proxy.enabled ? '已启用' : '未启用' }}</dd>
          </div>
          <div>
            <dt>运行状态</dt>
            <dd>{{ launcher?.selectedRunning ? '正在运行' : '未检测到运行中的所选宿主' }}</dd>
          </div>
        </dl>
        <div class="panel__actions">
          <button
            class="button button--primary"
            type="button"
            :disabled="busy || !launcher?.config.desktop.selectedExecutable"
            @click="act('desktop_launch')"
          >
            <NavIcon name="launch" />启动 ChatGPT 桌面端
          </button>
          <button class="button button--secondary" type="button" @click="navigate('integration')">
            配置桌面与 Hook<NavIcon name="arrow" />
          </button>
          <button
            class="button button--secondary"
            type="button"
            :disabled="focusBusy"
            @click="focusDesktop"
          >
            {{ focusBusy ? '正在切换…' : '切回桌面应用' }}
          </button>
        </div>
        <p v-if="focusFeedback" class="field__hint" role="status">{{ focusFeedback }}</p>
        <template #note>
          <template v-if="!launcher?.config.desktop.selectedExecutable">
            请先选择实际使用的 ChatGPT 桌面应用。
          </template>
        </template>
      </PanelSection>

      <PanelSection title="通知链路">
        <ol class="path-list">
          <li v-for="(step, index) in pathSteps" :key="step.title">
            <span class="path-list__index" aria-hidden="true">{{ index + 1 }}</span>
            <span class="status-card__text">
              <strong>{{ step.title }}</strong>
              <small class="truncate">{{ step.detail }}</small>
            </span>
          </li>
        </ol>
        <button
          class="button button--secondary button--wide"
          type="button"
          @click="navigate('notifications')"
        >
          配置 Relay 与通知<NavIcon name="arrow" />
        </button>
      </PanelSection>
    </div>

    <PanelSection title="最近投递">
      <template #actions>
        <button class="button button--ghost" type="button" @click="navigate('deliveries')">
          查看记录<NavIcon name="arrow" />
        </button>
      </template>
      <DeliveryTable v-if="deliveries.length" :items="deliveries" :limit="4" caption="最近投递" />
      <EmptyState
        v-else
        icon="deliveries"
        title="暂无投递"
        description="Codex 会话触发或发送测试通知后会显示在这里。"
        action-label="前往 Relay 与通知"
        @action="navigate('notifications')"
      />
    </PanelSection>
  </div>
</template>

<style scoped>
.status-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: var(--grid-gap-compact);
  margin: 0 0 var(--space-3);
  padding: 0;
  list-style: none;
}

.status-card {
  display: grid;
  width: 100%;
  min-width: 0;
  min-height: var(--status-card-min-height);
  grid-template-columns: var(--status-card-columns);
  align-items: start;
  gap: var(--space-2);
  padding: var(--space-3);
  border: 1px solid var(--border-default);
  border-radius: var(--panel-radius);
  background: var(--bg-surface);
  color: var(--text-primary);
  text-align: start;
  cursor: pointer;
  transition:
    background-color var(--duration-hover) var(--ease-out),
    border-color var(--duration-hover) var(--ease-out);
}

.status-card:hover {
  border-color: var(--border-strong);
  background: var(--bg-surface-hover);
}

.status-card__text {
  display: grid;
  min-width: 0;
  gap: var(--stack-gap-hair);
}

.status-card strong,
.path-list strong {
  font-size: var(--font-size-body);
  font-weight: var(--weight-semibold);
}

.status-card small,
.path-list small {
  color: var(--text-secondary);
  font-size: var(--font-size-micro);
  line-height: var(--line-micro);
}

.status-card .chip {
  grid-column: 2;
  justify-self: start;
}

.overview-grid {
  display: grid;
  grid-template-columns: var(--split-columns);
  gap: var(--space-3);
  margin-bottom: var(--space-3);
}

.quick-start {
  display: flex;
  min-height: var(--panel-min-height-tall);
  flex-direction: column;
}

.quick-start :deep(.panel__actions) {
  margin-top: auto;
}

.notification-path {
  min-height: var(--panel-min-height-tall);
}

.path-list {
  position: relative;
  display: grid;
  margin: 0;
  padding: var(--path-list-padding);
  list-style: none;
}

.path-list::before {
  position: absolute;
  top: var(--path-rail-inset-block);
  bottom: var(--path-rail-inset-block);
  inset-inline-start: var(--path-rail-inset-inline);
  width: 1px;
  background: var(--border-strong);
  content: '';
}

.path-list li {
  position: relative;
  display: grid;
  min-height: var(--path-step-min-height);
  grid-template-columns: var(--path-step-columns);
  align-items: center;
  gap: var(--grid-gap-compact);
}

.path-list__index {
  z-index: var(--z-raised);
  display: grid;
  width: var(--path-index-size);
  height: var(--path-index-size);
  place-items: center;
  border: 1px solid var(--border-strong);
  border-radius: 50%;
  background: var(--bg-surface);
  color: var(--text-secondary);
  font-family: var(--font-mono);
  font-size: var(--font-size-nano);
}

.activity-filters,
.activity-more {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
}

.activity-filters {
  margin-bottom: var(--space-2);
}

.activity-filter--active {
  border-color: var(--brand-primary);
  background: var(--brand-soft);
  color: var(--brand-primary);
}

.activity-filter .numeric {
  margin-inline-start: var(--space-1);
}

.activity-list {
  display: grid;
}

.activity-panel {
  order: -1;
  grid-row: 1;
}

.view--overview {
  display: grid;
  gap: var(--space-3);
}

/* 断点：CSS 媒体查询无法插值自定义属性，此处为唯一声明点 */
@media (max-width: 980px) {
  .status-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .overview-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}

@media (max-width: 720px) {
  .status-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
