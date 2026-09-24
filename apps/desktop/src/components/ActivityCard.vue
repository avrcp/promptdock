<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import StatusChip from './StatusChip.vue'
import type { ActivityDetail, ActivityItem } from '../desktop/activity'
import { deliveryLabel } from '../desktop/dictionaries'

function deliveryStateLabel(state: string, lastErrorCode: string | null): string {
  if (state === 'blocked_reconnect' && lastErrorCode === 'RELAY_RESULT_PAGES_UNAVAILABLE') {
    return '等待服务器结果页能力'
  }
  return deliveryLabel(state)
}

interface ActivityCardProps {
  item: ActivityItem
  detail?: ActivityDetail | null
  detailPending?: boolean
  actionPending?: boolean
}

const props = withDefaults(defineProps<Readonly<ActivityCardProps>>(), {
  detail: null,
  detailPending: false,
  actionPending: false,
})
const emit = defineEmits<{
  select: [runKey: string]
  close: []
  acknowledge: [item: ActivityItem]
  markSeen: [item: ActivityItem]
  openDeliveries: []
}>()

const stateLabel = computed(() => {
  const labels: Record<ActivityItem['phase'], string> = {
    started: '已开始观察',
    settling: '正在等待完成',
    ended_observed: '已观察到结束',
    interrupted: '已中断',
    failed_observed: '已观察到失败',
    cancelled: '已取消',
    unknown: '状态未知',
  }
  if (props.item.attention?.historical) return '历史注意事项'
  if (
    props.item.attention &&
    props.item.attention.revision <= props.item.attention.acknowledgedRevision
  )
    return '已知晓'
  return props.item.attention?.label || labels[props.item.phase]
})
const stateTone = computed(() => {
  if (
    props.item.attention &&
    !props.item.attention.historical &&
    props.item.attention.revision > props.item.attention.acknowledgedRevision
  )
    return 'warning'
  if (props.item.delivery?.lastErrorCode) return 'warning'
  if (props.item.result || props.item.phase === 'ended_observed') return 'success'
  return 'neutral'
})
const occurredAt = computed(() => new Date(props.item.lastObservedAt).toLocaleString())
const canAcknowledge = computed(
  () =>
    props.item.attention !== null &&
    props.item.attention.revision > props.item.attention.acknowledgedRevision,
)
const canMarkSeen = computed(
  () =>
    props.item.result !== null &&
    props.item.result.resultRevision > props.item.result.seenResultRevision,
)
const heldUntil = computed(() =>
  props.item.delivery?.heldUntil ? new Date(props.item.delivery.heldUntil).toLocaleString() : null,
)
const canOpenResult = computed(
  () =>
    props.item.result?.pageState === 'available' && (props.item.result.expiresAt ?? 0) > Date.now(),
)
async function openResult() {
  if (!props.item.result || !canOpenResult.value || opening.value) return
  const observed = props.item
  opening.value = true
  openError.value = ''
  try {
    await invoke('desktop_result_open', { id: observed.result!.outboxId })
    emit('markSeen', observed)
  } catch {
    openError.value = '结果未能打开，请检查页面状态后重试。'
  } finally {
    opening.value = false
  }
}
const opening = ref(false)
const openError = ref('')
const summaryButton = ref<HTMLButtonElement | null>(null)
async function closeDetail() {
  emit('close')
  await nextTick()
  summaryButton.value?.focus()
}
function pageLabel(state: string | null) {
  return (
    (
      {
        available: '可查看',
        expired: '已过期',
        revoked: '已撤销',
        content_unavailable: '正文不可用',
      } as Record<string, string>
    )[state ?? ''] ?? '等待服务器接管'
  )
}
function eventLabel(kind: string) {
  return (
    (
      {
        run_started: '开始观察',
        run_settling: '收到结束观察',
        run_completed: '结束确认',
        run_failed: '失败观察',
        run_interrupted: '中断观察',
        run_cancelled: '取消观察',
        attention_required: '权限请求',
        output_observed: '结果已保存',
        test: '测试通知',
      } as Record<string, string>
    )[kind] ?? '其他事件'
  )
}
</script>

<template>
  <article class="activity-card" :data-run-key="item.runKey">
    <button
      ref="summaryButton"
      class="activity-card__summary"
      type="button"
      @click="emit('select', item.runKey)"
    >
      <span class="activity-card__copy">
        <strong>{{ item.displayTitle || stateLabel }}</strong>
        <small>{{ item.workspaceLabel }} · 最近观察：{{ occurredAt }}</small>
      </span>
      <StatusChip :tone="stateTone" :label="stateLabel" />
    </button>

    <p v-if="item.result" class="field__hint">
      结果版本 {{ item.result.resultRevision }} · {{ pageLabel(item.result.pageState) }}
    </p>
    <p v-if="item.delivery" class="field__hint">
      {{ deliveryStateLabel(item.delivery.state, item.delivery.lastErrorCode)
      }}<template v-if="heldUntil">，暂缓至 {{ heldUntil }}</template>
    </p>
    <p v-if="openError" role="alert">{{ openError }}</p>

    <div class="activity-card__actions">
      <button
        v-if="canAcknowledge"
        class="button button--primary"
        type="button"
        :disabled="actionPending"
        @click="emit('acknowledge', item)"
      >
        确认已知晓
      </button>
      <button
        v-if="canMarkSeen"
        class="button button--secondary"
        type="button"
        :disabled="actionPending"
        @click="emit('markSeen', item)"
      >
        标记结果已查看
      </button>
      <button
        v-if="canOpenResult"
        class="button button--secondary"
        type="button"
        :disabled="opening || actionPending"
        @click="openResult"
      >
        打开结果
      </button>
      <button class="button button--secondary" type="button" @click="emit('select', item.runKey)">
        {{ detail?.item.runKey === item.runKey ? '更新详情' : '查看元数据' }}
      </button>
    </div>

    <section
      v-if="detail?.item.runKey === item.runKey"
      class="activity-card__detail"
      aria-label="活动元数据"
      @keydown.esc.stop="closeDetail"
    >
      <div class="activity-card__detail-heading">
        <strong>活动时间线</strong>
        <button class="button button--ghost" type="button" @click="closeDetail">收起</button>
      </div>
      <p v-if="detailPending" role="status">正在读取活动元数据…</p>
      <ol v-else class="activity-timeline">
        <li v-for="event in detail.events" :key="event.id">
          <strong>{{ eventLabel(event.kind) }}</strong>
          <time :datetime="new Date(event.observedAt).toISOString()">{{
            new Date(event.observedAt).toLocaleString()
          }}</time>
        </li>
      </ol>
      <div v-if="detail.deliveries.length" class="activity-deliveries">
        <strong>结果与投递</strong>
        <ul>
          <li v-for="delivery in detail.deliveries" :key="delivery.id">
            {{ eventLabel(delivery.kind) }}：{{ deliveryLabel(delivery.state) }}
            <template v-if="delivery.pageState"> （{{ pageLabel(delivery.pageState) }}） </template>
          </li>
        </ul>
        <button class="button button--secondary" type="button" @click="emit('openDeliveries')">
          查看投递记录
        </button>
      </div>
      <p v-if="!detail.deliveries.length && !detailPending" class="field__hint">
        尚未形成可展示的投递元数据。
      </p>
    </section>
  </article>
</template>

<style scoped>
.activity-card {
  display: grid;
  gap: var(--space-2);
  padding: var(--space-3) 0;
  border-bottom: 1px solid var(--border-default);
}
.activity-card:last-child {
  border-bottom: 0;
}
.activity-card__summary {
  display: flex;
  min-width: 0;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
  padding: 0;
  border: 0;
  background: transparent;
  color: var(--text-primary);
  text-align: start;
  cursor: pointer;
}
.activity-card__copy {
  display: grid;
  min-width: 0;
  gap: var(--stack-gap-hair);
}
.activity-card__copy small,
.activity-timeline time {
  color: var(--text-secondary);
  font-size: var(--font-size-micro);
}
.activity-card__actions,
.activity-card__detail-heading {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
}
.activity-card__detail-heading {
  justify-content: space-between;
}
.activity-card__detail {
  display: grid;
  gap: var(--space-2);
  padding: var(--space-3);
  background: var(--bg-canvas);
}
.activity-timeline,
.activity-deliveries ul {
  display: grid;
  gap: var(--space-2);
  margin: 0;
  padding-inline-start: var(--space-4);
}
.activity-timeline li {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
.activity-deliveries {
  display: grid;
  gap: var(--space-2);
}
@media (max-width: 520px) {
  .activity-card__summary {
    align-items: start;
    flex-direction: column;
  }
}
</style>
