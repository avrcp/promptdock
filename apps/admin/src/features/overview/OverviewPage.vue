<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import { useRouter } from 'vue-router'
import {
  RefreshCw,
  Activity,
  MessageSquare,
  Smartphone,
  ListOrdered,
  AlertTriangle,
} from 'lucide-vue-next'

import StatCard from '@/components/StatCard.vue'
import StatusChip from '@/components/StatusChip.vue'
import AdminErrorAlert from '@/components/AdminErrorAlert.vue'
import EmptyState from '@/components/EmptyState.vue'
import SkeletonBlock from '@/components/SkeletonBlock.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import AppButton from '@/components/AppButton.vue'
import { useAsyncResource } from '@/composables/useAsyncResource'
import { useAdminRepository } from '@/composables/useAdminRepository'
import { useNow } from '@/composables/useNow'
import { componentHealthVisual } from '@/features/devices/device-status'
import { topAlerts, presentAlert } from './alert-presentation'

const adminRepository = useAdminRepository()
const { now } = useNow()
const router = useRouter()

function goTo(path: string): void {
  void router.push(path)
}

const overviewResource = useAsyncResource<
  Awaited<ReturnType<typeof adminRepository.read.getOverview>>
>({
  fetcher: (signal) => adminRepository.read.getOverview({ signal }),
})

const wechatResource = useAsyncResource<
  Awaited<ReturnType<typeof adminRepository.read.getWechatStatus>>
>({
  fetcher: (signal) => adminRepository.read.getWechatStatus({ signal }),
})

// Shared freshness window for the Overview panel. 60 s matches the implicit
// expectation of an operator glancing at this page between tasks.
const OVERVIEW_STALE_THRESHOLD_MS = 60_000

const refreshing = computed(
  () => overviewResource.refreshing.value || wechatResource.refreshing.value,
)

const overviewData = computed(() => overviewResource.data.value)
const overviewError = computed(() => overviewResource.error.value)
const overviewLoading = computed(
  () => overviewResource.loading.value && overviewResource.data.value === null,
)
const wechatData = computed(() => wechatResource.data.value)
const wechatError = computed(() => wechatResource.error.value)
const wechatLoading = computed(
  () => wechatResource.loading.value && wechatResource.data.value === null,
)

/**
 * True when the resource has data but the most recent attempt failed, or
 * the cached data is older than the freshness window.  The panel UI uses
 * this to render "showing data from X ago" instead of treating the screen as
 * up-to-date.
 */
const overviewStale = computed(() => {
  if (overviewResource.data.value === null) return false
  if (overviewResource.error.value !== null) return true
  return overviewResource.isStale(OVERVIEW_STALE_THRESHOLD_MS, now.value)
})
const wechatStale = computed(() => {
  if (wechatResource.data.value === null) return false
  if (wechatResource.error.value !== null) return true
  return wechatResource.isStale(OVERVIEW_STALE_THRESHOLD_MS, now.value)
})

const relayVisual = computed(() => {
  const data = overviewData.value
  if (!data) {
    return { tone: 'muted' as const, label: '读取中', hint: '' }
  }
  const visual = componentHealthVisual(data.relay.health)
  return {
    tone: visual.tone,
    label: visual.label,
    hint: `版本 ${data.relay.version}`,
  }
})

const wechatVisual = computed(() => {
  const data = wechatData.value
  if (!data) {
    return { tone: 'muted' as const, label: '读取中', hint: '' }
  }
  return {
    tone:
      data.state === 'ready'
        ? ('success' as const)
        : data.state === 'disconnected' ||
            data.state === 'needs_reconnect' ||
            data.state === 'credentials_unreadable'
          ? ('danger' as const)
          : ('warning' as const),
    label:
      data.state === 'ready'
        ? '已就绪'
        : data.state === 'disconnected'
          ? '未连接'
          : data.state === 'degraded'
            ? '不稳定'
            : data.state === 'needs_reconnect'
              ? '需要重连'
              : data.state === 'credentials_unreadable'
                ? '凭据不可读'
                : data.state === 'connected_awaiting_activation'
                  ? '等待激活'
                  : '未知',
    hint: data.accountHint ? `账号 ${data.accountHint}` : '未配置账号',
  }
})

const deviceSummary = computed(() => overviewData.value?.devices ?? null)
const queueSummary = computed(() => overviewData.value?.queue ?? null)
const currentAlerts = computed(() => overviewData.value?.currentAlerts ?? [])

// First-layer "now-what" list.  We pick the top 3 by severity so the
// operator can decide in seconds without scanning the full alerts panel.
const topAttentionAlerts = computed(() => topAlerts(currentAlerts.value, 3))

const deviceLabel = computed(() => {
  if (!deviceSummary.value) return '—'
  return `${deviceSummary.value.onlineTotal} / ${deviceSummary.value.enabledTotal}`
})

const queueLabel = computed(() => {
  if (!queueSummary.value) return '—'
  const q = queueSummary.value
  return `${q.pending} 待发 · ${q.failed} 失败`
})

const gatewayConnections = computed(() => overviewData.value?.gatewayConnections ?? [])

const overviewHasError = computed(() => overviewError.value !== null && overviewData.value === null)
const wechatHasError = computed(() => wechatError.value !== null && wechatData.value === null)

const refreshingStatus = computed<string>(() => {
  // Prefer the success timestamp we track ourselves; it survives partial
  // failures where generatedAt is missing on one of the resources.
  const lastSuccess = overviewResource.lastSuccessAt.value ?? wechatResource.lastSuccessAt.value
  if (lastSuccess === null) return '最近刷新：—'
  const stale = overviewStale.value || wechatStale.value
  return stale
    ? `数据可能已过期 · ${humanizeAgo(lastSuccess, now.value)}`
    : `最近刷新：${humanizeAgo(lastSuccess, now.value)}`
})

function humanizeAgo(ts: number, current: number = now.value): string {
  const diff = current - ts
  if (diff < 0) return '刚刚'
  if (diff < 5_000) return '刚刚'
  if (diff < 60_000) return `${Math.floor(diff / 1_000)} 秒前`
  if (diff < 60 * 60_000) return `${Math.floor(diff / 60_000)} 分钟前`
  if (diff < 24 * 60 * 60_000) return `${Math.floor(diff / (60 * 60_000))} 小时前`
  return `${Math.floor(diff / (24 * 60 * 60_000))} 天前`
}

const refreshAnnouncement = ref('')
let announceTimer: ReturnType<typeof setTimeout> | null = null

async function refreshAll(): Promise<void> {
  if (refreshing.value) return
  // Each refresh() returns a ResourceRunResult describing THIS attempt. The
  // promise is intentionally non-rejecting; truth must come from the result
  // object, otherwise partial failures get reported as "刷新完成".
  const [overviewResult, wechatResult] = await Promise.all([
    overviewResource.refresh('refresh'),
    wechatResource.refresh('refresh'),
  ])
  const succeeded = [overviewResult, wechatResult].filter((r) => r.ok).length
  const failed = [overviewResult, wechatResult].filter((r) => !r.ok && !r.aborted).length
  const aborted = [overviewResult, wechatResult].filter((r) => r.aborted).length
  const attempted = succeeded + failed
  if (failed === 0 && aborted === 0) {
    refreshAnnouncement.value = `刷新完成，${succeeded} 个分区已更新。`
  } else if (succeeded === 0) {
    refreshAnnouncement.value =
      aborted > 0 ? '刷新被新请求打断，未产生新结果。' : '刷新失败，所有分区均未能更新。'
  } else {
    refreshAnnouncement.value = `刷新部分完成：${succeeded} 个分区已更新，${failed} 个分区失败，${attempted} 个分区已尝试。`
  }
  if (announceTimer) clearTimeout(announceTimer)
  announceTimer = setTimeout(() => {
    refreshAnnouncement.value = ''
  }, 6_000)
}

onBeforeUnmount(() => {
  if (announceTimer) clearTimeout(announceTimer)
})
</script>

<template>
  <div class="overview">
    <div class="overview__toolbar" aria-label="总览数据操作">
      <span
        class="overview__refresh-status tabular"
        :class="{ 'overview__refresh-status--stale': overviewStale || wechatStale }"
        data-testid="overview-refresh-status"
      >
        <span
          v-if="overviewStale || wechatStale"
          class="overview__freshness-dot"
          aria-hidden="true"
        />
        {{ refreshingStatus }}
      </span>
      <AppButton
        variant="secondary"
        size="sm"
        :loading="refreshing"
        aria-label="刷新总览"
        data-testid="overview-refresh"
        @click="refreshAll"
      >
        <RefreshCw :size="14" aria-hidden="true" />
        <span>刷新</span>
      </AppButton>
    </div>

    <div
      class="overview__metric-strip"
      role="region"
      aria-label="核心运行状态"
      data-testid="overview-metric-strip"
    >
      <StatCard
        label="Relay"
        :value="relayVisual.label"
        :hint="relayVisual.hint"
        :tone="relayVisual.tone"
        :icon="Activity"
        :loading="overviewLoading"
        data-testid="stat-relay"
      />
      <StatCard
        label="微信通道"
        :value="wechatVisual.label"
        :hint="wechatVisual.hint"
        :tone="wechatVisual.tone"
        :icon="MessageSquare"
        :loading="wechatLoading"
        data-testid="stat-wechat"
      />
      <StatCard
        label="在线设备"
        :value="deviceLabel"
        :hint="deviceSummary ? `已启用 ${deviceSummary.enabledTotal} 台` : '加载中'"
        :tone="(deviceSummary?.onlineTotal ?? 0) > 0 ? 'success' : 'muted'"
        :icon="Smartphone"
        :loading="overviewLoading"
        data-testid="stat-devices"
      />
      <StatCard
        label="待处理队列"
        :value="queueLabel"
        :hint="
          queueSummary ? `${queueSummary.retrying} 重试 · ${queueSummary.blocked} 阻塞` : '加载中'
        "
        :tone="(queueSummary?.failed ?? 0) > 0 ? 'danger' : 'info'"
        :icon="ListOrdered"
        :loading="overviewLoading"
        data-testid="stat-queue"
      />
    </div>

    <section
      v-if="topAttentionAlerts.length > 0"
      class="overview__attention"
      aria-labelledby="overview-attention-title"
      data-testid="overview-attention"
    >
      <div class="overview__attention-head">
        <h2 id="overview-attention-title" class="overview__attention-title">现在该处理</h2>
        <span class="overview__attention-count tabular">
          优先显示 {{ topAttentionAlerts.length }} / 共 {{ currentAlerts.length }} 项
        </span>
      </div>
      <ul class="overview__attention-list">
        <li
          v-for="alert in topAttentionAlerts"
          :key="alert.id"
          class="overview__attention-item"
          :class="`overview__attention-item--${alert.severity}`"
        >
          <StatusChip
            :tone="
              alert.severity === 'error'
                ? 'danger'
                : alert.severity === 'warning'
                  ? 'warning'
                  : 'info'
            "
            :label="alert.code"
          />
          <span class="overview__attention-msg" :title="presentAlert(alert).title">
            {{ presentAlert(alert).title }}
          </span>
          <span class="overview__attention-time"><TimeAgo :timestamp="alert.observedAt" /></span>
          <AppButton
            v-if="presentAlert(alert).targetRoute"
            variant="secondary"
            size="sm"
            :aria-label="`${presentAlert(alert).actionLabel}：${presentAlert(alert).title}`"
            @click="goTo(presentAlert(alert).targetRoute!)"
          >
            {{ presentAlert(alert).actionLabel }}
          </AppButton>
        </li>
      </ul>
    </section>

    <AdminErrorAlert
      v-if="overviewHasError"
      :error="overviewError"
      testid="overview-error"
      @retry="refreshAll"
    />
    <AdminErrorAlert
      v-else-if="wechatHasError"
      :error="wechatError"
      testid="wechat-error"
      @retry="refreshAll"
    />

    <div class="overview__workspace">
      <section class="overview__panel" aria-labelledby="panel-wechat">
        <header class="overview__panel-head">
          <h2 id="panel-wechat" class="overview__panel-title">微信通道详情</h2>
          <span v-if="wechatStale" class="overview__panel-warning">
            <span class="overview__freshness-dot" aria-hidden="true" />
            数据可能已过期
          </span>
        </header>
        <div class="overview__panel-body">
          <div
            v-if="wechatLoading"
            class="overview__skeleton-stack"
            role="status"
            aria-live="polite"
          >
            <span class="sr-only">正在加载微信通道</span>
            <SkeletonBlock variant="text" width="60%" />
            <SkeletonBlock variant="text" width="40%" />
          </div>
          <dl v-else-if="wechatData" class="overview__definition-list">
            <div>
              <dt>账号</dt>
              <dd>{{ wechatData.accountHint ?? '未配置' }}</dd>
            </div>
            <div>
              <dt>最近 Poll</dt>
              <dd><TimeAgo :timestamp="wechatData.lastPollAt" /></dd>
            </div>
            <div>
              <dt>最近 Context</dt>
              <dd><TimeAgo :timestamp="wechatData.lastContextAt" /></dd>
            </div>
            <div>
              <dt>最近错误</dt>
              <dd class="overview__technical-value">{{ wechatData.lastErrorCode ?? '—' }}</dd>
            </div>
          </dl>
        </div>
      </section>

      <section class="overview__panel" aria-labelledby="panel-gateway">
        <header class="overview__panel-head">
          <h2 id="panel-gateway" class="overview__panel-title">Gateway 在线连接</h2>
          <span class="overview__panel-count tabular">{{ gatewayConnections.length }}</span>
        </header>
        <div class="overview__panel-body overview__panel-body--flush">
          <div
            v-if="overviewLoading"
            class="overview__skeleton-stack overview__skeleton-stack--padded"
            role="status"
            aria-live="polite"
          >
            <span class="sr-only">正在加载 Gateway 连接</span>
            <SkeletonBlock variant="text" width="80%" />
            <SkeletonBlock variant="text" width="60%" />
            <SkeletonBlock variant="text" width="70%" />
          </div>
          <EmptyState
            v-else-if="gatewayConnections.length === 0"
            variant="bare"
            title="当前没有 Gateway 连接"
            description="设备上线后，Gateway 连接会出现在此处。"
          />
          <ul v-else class="overview__list">
            <li v-for="conn in gatewayConnections" :key="conn.deviceId" class="overview__list-item">
              <div class="overview__list-row">
                <span class="overview__list-name" :title="conn.deviceName">
                  {{ conn.deviceName }}
                </span>
                <span class="overview__connection-state">
                  <span
                    class="overview__connection-dot"
                    :class="{ 'overview__connection-dot--online': conn.connected }"
                    aria-hidden="true"
                  />
                  {{ conn.connected ? '已连接' : '未连接' }}
                </span>
              </div>
              <div class="overview__list-meta tabular">
                <span>generation {{ conn.generation }}</span>
                <span>客户端 {{ conn.clientVersion ?? '未知' }}</span>
                <span>
                  心跳
                  <TimeAgo :timestamp="conn.lastHeartbeatAt" />
                </span>
              </div>
            </li>
          </ul>
        </div>
      </section>

      <section class="overview__panel" aria-labelledby="panel-queue">
        <header class="overview__panel-head">
          <h2 id="panel-queue" class="overview__panel-title">队列摘要</h2>
        </header>
        <div class="overview__panel-body overview__panel-body--flush">
          <div
            v-if="overviewLoading"
            class="overview__skeleton-stack overview__skeleton-stack--padded"
            role="status"
            aria-live="polite"
          >
            <span class="sr-only">正在加载队列摘要</span>
            <SkeletonBlock variant="text" width="70%" />
            <SkeletonBlock variant="text" width="60%" />
          </div>
          <template v-else-if="queueSummary">
            <div class="overview__metrics">
              <div class="overview__metric">
                <span class="overview__metric-label">待发送</span>
                <span class="overview__metric-value tabular">{{ queueSummary.pending }}</span>
              </div>
              <div class="overview__metric">
                <span class="overview__metric-label">发送中</span>
                <span class="overview__metric-value tabular">{{ queueSummary.sending }}</span>
              </div>
              <div class="overview__metric">
                <span class="overview__metric-label">重试中</span>
                <span class="overview__metric-value overview__metric-value--warning tabular">
                  {{ queueSummary.retrying }}
                </span>
              </div>
              <div class="overview__metric">
                <span class="overview__metric-label">阻塞</span>
                <span class="overview__metric-value overview__metric-value--danger tabular">
                  {{ queueSummary.blocked }}
                </span>
              </div>
              <div class="overview__metric">
                <span class="overview__metric-label">失败</span>
                <span class="overview__metric-value overview__metric-value--danger tabular">
                  {{ queueSummary.failed }}
                </span>
              </div>
            </div>
          </template>
        </div>
      </section>

      <section class="overview__panel" aria-labelledby="panel-alerts">
        <header class="overview__panel-head">
          <h2 id="panel-alerts" class="overview__panel-title">当前问题</h2>
          <span class="overview__panel-count tabular">{{ currentAlerts.length }}</span>
        </header>
        <div class="overview__panel-body overview__panel-body--flush">
          <EmptyState
            v-if="!overviewLoading && currentAlerts.length === 0"
            variant="bare"
            :icon="AlertTriangle"
            title="当前没有需要处理的问题"
            description="Relay 检测到需要关注的标准化事件后，会在这里提供完整诊断信息。"
          />
          <ul v-else-if="currentAlerts.length > 0" class="overview__list">
            <li
              v-for="issue in currentAlerts"
              :key="issue.id"
              class="overview__issue"
              :class="`overview__issue--${issue.severity}`"
            >
              <div class="overview__list-row">
                <span class="overview__list-name" :title="issue.message">{{ issue.message }}</span>
                <StatusChip
                  :tone="
                    issue.severity === 'error'
                      ? 'danger'
                      : issue.severity === 'warning'
                        ? 'warning'
                        : 'info'
                  "
                  :label="issue.code"
                />
              </div>
              <div class="overview__list-meta">
                <span>组件 {{ issue.component }}</span>
                <span><TimeAgo :timestamp="issue.observedAt" /></span>
              </div>
            </li>
          </ul>
        </div>
      </section>
    </div>

    <p class="sr-only" role="status" aria-live="polite" data-testid="overview-live-region">
      {{ refreshAnnouncement }}
    </p>
  </div>
</template>

<style scoped>
.overview {
  min-width: 0;
  container-type: inline-size;
}

.overview__toolbar {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: var(--pd-space-12);
  min-height: var(--pd-control-height-md);
  margin-bottom: var(--pd-space-12);
}

.overview__refresh-status {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-4);
  color: var(--pd-text-subtle);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-11);
  white-space: nowrap;
}

.overview__refresh-status--stale,
.overview__panel-warning {
  color: var(--pd-feedback-warning);
}

.overview__freshness-dot,
.overview__connection-dot {
  width: 6px;
  height: 6px;
  flex: 0 0 auto;
  border-radius: 50%;
  background: var(--pd-feedback-warning);
}

.overview__metric-strip {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: 1px;
  margin-bottom: var(--pd-space-16);
  overflow: hidden;
  background: var(--pd-border-separator);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.overview__attention {
  margin-bottom: var(--pd-space-16);
  overflow: hidden;
  background: var(--pd-container-panel-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.overview__attention-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-12);
  min-height: var(--pd-panel-header-height);
  padding: 0 var(--pd-space-12);
  border-bottom: 1px solid var(--pd-border-separator);
}

.overview__attention-title {
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-semibold);
}

.overview__attention-count {
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-11);
}

.overview__attention-list {
  display: grid;
}

.overview__attention-item {
  display: grid;
  grid-template-columns: auto 1fr auto auto;
  align-items: center;
  gap: var(--pd-space-12);
  min-height: var(--pd-table-row-height);
  padding: var(--pd-space-4) var(--pd-space-12) var(--pd-space-4) var(--pd-space-8);
  border-bottom: 1px solid var(--pd-border-separator);
  border-inline-start: 2px solid var(--pd-feedback-info);
}

.overview__attention-item:last-child {
  border-bottom: 0;
}

.overview__attention-item--warning {
  border-inline-start-color: var(--pd-feedback-warning);
}

.overview__attention-item--error {
  border-inline-start-color: var(--pd-feedback-danger);
}

.overview__attention-msg {
  min-width: 0;
  overflow: hidden;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.overview__attention-time {
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-11);
}

.overview__workspace {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--pd-space-12);
}

.overview__panel {
  min-width: 0;
  overflow: hidden;
  background: var(--pd-container-panel-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.overview__panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-12);
  min-height: var(--pd-panel-header-height);
  padding: 0 var(--pd-space-12);
  border-bottom: 1px solid var(--pd-border-separator);
}

.overview__panel-title {
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-semibold);
}

.overview__panel-warning,
.overview__panel-count {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-4);
  font-size: var(--pd-font-size-11);
}

.overview__panel-count {
  color: var(--pd-text-subtle);
  font-family: var(--pd-font-code);
}

.overview__panel-body {
  padding: var(--pd-space-12);
}

.overview__panel-body--flush {
  padding: 0;
}

.overview__skeleton-stack {
  display: grid;
  gap: var(--pd-space-8);
}

.overview__skeleton-stack--padded {
  padding: var(--pd-space-12);
}

.overview__definition-list {
  margin: 0;
}

.overview__definition-list > div {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-16);
  min-height: var(--pd-control-height-md);
  border-bottom: 1px solid var(--pd-border-separator);
  font-size: var(--pd-font-size-13);
}

.overview__definition-list > div:last-child {
  border-bottom: 0;
}

.overview__definition-list dt {
  flex: 0 0 auto;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
}

.overview__definition-list dd {
  min-width: 0;
  margin: 0;
  overflow-wrap: anywhere;
  color: var(--pd-text-default);
  text-align: end;
}

.overview__technical-value {
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-12);
}

.overview__list {
  display: grid;
}

.overview__list-item {
  min-width: 0;
  padding: var(--pd-space-8) var(--pd-space-12);
  border-bottom: 1px solid var(--pd-border-separator);
}

.overview__list-item:last-child,
.overview__issue:last-child {
  border-bottom: 0;
}

.overview__list-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-12);
}

.overview__list-name {
  min-width: 0;
  overflow: hidden;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-medium);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.overview__connection-state {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-4);
  flex: 0 0 auto;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
}

.overview__connection-dot {
  background: var(--pd-text-hint);
}

.overview__connection-dot--online {
  background: var(--pd-feedback-success);
}

.overview__list-meta {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--pd-space-4) var(--pd-space-12);
  margin-top: var(--pd-space-4);
  color: var(--pd-text-subtle);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-11);
}

.overview__metrics {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 13rem), 1fr));
}

.overview__metric {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-4);
  min-width: 0;
  padding: var(--pd-space-12);
  border-inline-end: 1px solid var(--pd-border-separator);
}

.overview__metric:last-child {
  border-inline-end: 0;
}

.overview__metric-label {
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-11);
}

.overview__metric-value {
  color: var(--pd-text-default);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-16);
  font-weight: var(--pd-font-weight-semibold);
}

.overview__metric-value--warning {
  color: var(--pd-feedback-warning);
}

.overview__metric-value--danger {
  color: var(--pd-feedback-danger);
}

.overview__issue {
  padding: var(--pd-space-8) var(--pd-space-12) var(--pd-space-8) var(--pd-space-8);
  border-bottom: 1px solid var(--pd-border-separator);
  border-inline-start: 2px solid var(--pd-feedback-info);
}

.overview__issue--warning {
  border-inline-start-color: var(--pd-feedback-warning);
}

.overview__issue--error {
  border-inline-start-color: var(--pd-feedback-danger);
}

@container (max-width: 44rem) {
  .overview__workspace {
    grid-template-columns: minmax(0, 1fr);
  }
}

@container (min-width: 30rem) {
  .overview__metric-strip {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@container (min-width: 48rem) {
  .overview__metric-strip {
    grid-template-columns: repeat(4, minmax(0, 1fr));
  }
}

@media (max-width: 767px) {
  .overview__attention-item {
    grid-template-columns: auto minmax(0, 1fr) auto;
  }

  .overview__attention-time {
    grid-column: 2;
  }

  .overview__attention-item :deep(.app-button) {
    grid-column: 3;
    grid-row: 1 / span 2;
  }
}
</style>
