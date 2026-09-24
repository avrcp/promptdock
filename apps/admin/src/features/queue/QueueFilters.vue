<script setup lang="ts">
import {
  deliveryOriginSchema,
  deliveryStateSchema,
  inboundCommandSchema,
  inboundStateSchema,
  type DeliveryOrigin,
  type DeliveryState,
  type InboundCommand,
  type InboundState,
} from '@/contracts/queue'
import AppButton from '@/components/AppButton.vue'
import {
  deliveryOriginVisual,
  deliveryStateVisual,
  inboundCommandVisual,
  inboundStateVisual,
  replyStateVisual,
} from './queue-status'
import type { QueueTab } from './useQueueController'

defineProps<{
  tab: QueueTab
  freshness: string
  freshnessTestid: string
  freshnessStale: boolean
  resultCount: number
  total: number | null
  hasFilters: boolean
}>()

const emit = defineEmits<{
  clearFilters: []
}>()

const deliveryState = defineModel<DeliveryState | null>('deliveryState', { default: null })
const deliveryOrigin = defineModel<DeliveryOrigin | null>('deliveryOrigin', { default: null })
const replyState = defineModel<DeliveryState | null>('replyState', { default: null })
const inboundState = defineModel<InboundState | null>('inboundState', { default: null })
const inboundCommand = defineModel<InboundCommand | null>('inboundCommand', { default: null })

const deliveryStates = deliveryStateSchema.options
const deliveryOrigins = deliveryOriginSchema.options
const inboundStates = inboundStateSchema.options
const inboundCommands = inboundCommandSchema.options

const tabLabels: Record<QueueTab, string> = {
  deliveries: '通知投递',
  replies: '交互回复',
  inbound: '入站命令',
}
</script>

<template>
  <div class="queue-filters" role="region" :aria-label="`${tabLabels[tab]} 筛选`">
    <div class="queue-filters__fields">
      <template v-if="tab === 'deliveries'">
        <label class="queue-filters__field">
          <span>状态</span>
          <select
            v-model="deliveryState"
            data-testid="delivery-state-filter"
            aria-label="按状态筛选投递"
          >
            <option :value="null">全部</option>
            <option v-for="state in deliveryStates" :key="state" :value="state">
              {{ deliveryStateVisual(state).label }}
            </option>
          </select>
        </label>
        <label class="queue-filters__field">
          <span>来源</span>
          <select
            v-model="deliveryOrigin"
            data-testid="delivery-origin-filter"
            aria-label="按来源筛选投递"
          >
            <option :value="null">全部</option>
            <option v-for="origin in deliveryOrigins" :key="origin" :value="origin">
              {{ deliveryOriginVisual(origin) }}
            </option>
          </select>
        </label>
      </template>
      <label v-else-if="tab === 'replies'" class="queue-filters__field">
        <span>状态</span>
        <select
          v-model="replyState"
          data-testid="reply-state-filter"
          aria-label="按状态筛选交互回复"
        >
          <option :value="null">全部</option>
          <option v-for="state in deliveryStates" :key="state" :value="state">
            {{ replyStateVisual(state).label }}
          </option>
        </select>
      </label>
      <template v-else>
        <label class="queue-filters__field">
          <span>命令</span>
          <select
            v-model="inboundCommand"
            data-testid="inbound-command-filter"
            aria-label="按命令类型筛选入站命令"
          >
            <option :value="null">全部</option>
            <option v-for="command in inboundCommands" :key="command" :value="command">
              {{ inboundCommandVisual(command) }}
            </option>
          </select>
        </label>
        <label class="queue-filters__field">
          <span>状态</span>
          <select
            v-model="inboundState"
            data-testid="inbound-state-filter"
            aria-label="按状态筛选入站命令"
          >
            <option :value="null">全部</option>
            <option v-for="state in inboundStates" :key="state" :value="state">
              {{ inboundStateVisual(state).label }}
            </option>
          </select>
        </label>
      </template>
    </div>
    <div class="queue-filters__metadata">
      <span class="queue-filters__count tabular" aria-live="polite">
        {{ total === null ? `本页 ${resultCount} 条` : `共 ${total} 条` }}
      </span>
      <span class="queue-filters__divider" aria-hidden="true" />
      <span
        class="queue-filters__freshness"
        :class="{ 'queue-filters__freshness--stale': freshnessStale }"
        :data-testid="freshnessTestid"
      >
        <span class="queue-filters__freshness-dot" aria-hidden="true" />
        {{ freshness }}
      </span>
      <AppButton
        v-if="hasFilters"
        variant="ghost"
        size="sm"
        data-testid="queue-clear-filters"
        @click="emit('clearFilters')"
      >
        清空筛选
      </AppButton>
    </div>
  </div>
</template>

<style scoped>
.queue-filters {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-12);
  min-height: var(--pd-control-height-touch);
  padding: var(--pd-space-8) var(--pd-space-12);
  margin-bottom: var(--pd-space-8);
  flex-wrap: wrap;
  background: var(--pd-container-header-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.queue-filters__fields {
  display: flex;
  gap: var(--pd-space-8);
  flex-wrap: wrap;
  align-items: center;
}

.queue-filters__field {
  display: inline-flex;
  flex-direction: column;
  gap: var(--pd-space-2);
  min-width: 0;
  font-size: var(--pd-font-size-11);
  color: var(--pd-text-subtle);
}

.queue-filters select {
  min-width: 140px;
  min-height: var(--pd-control-height-md);
  padding: 0 var(--pd-space-8);
  background: var(--pd-control-bg);
  color: var(--pd-text-default);
  border: 1px solid var(--pd-control-border-default);
  border-radius: var(--pd-radius-sm);
  font-size: var(--pd-font-size-13);
}

.queue-filters select:focus-visible {
  border-color: var(--pd-border-focus);
  outline: 2px solid var(--pd-border-focus);
  outline-offset: 0;
}

.queue-filters__metadata {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: var(--pd-space-8);
  min-width: 0;
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-11);
}

.queue-filters__count,
.queue-filters__freshness {
  white-space: nowrap;
}

.queue-filters__freshness {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-4);
  font-variant-numeric: tabular-nums;
}

.queue-filters__freshness-dot {
  width: 6px;
  height: 6px;
  background: var(--pd-feedback-success);
  border-radius: 50%;
}

.queue-filters__freshness--stale {
  color: var(--pd-feedback-warning);
}

.queue-filters__freshness--stale .queue-filters__freshness-dot {
  background: var(--pd-feedback-warning);
}

.queue-filters__divider {
  width: 1px;
  height: var(--pd-space-12);
  background: var(--pd-border-separator);
}

@media (max-width: 767px), (pointer: coarse) {
  .queue-filters,
  .queue-filters__fields {
    width: 100%;
  }

  .queue-filters__field,
  .queue-filters select {
    flex: 1;
    min-width: 0;
  }

  .queue-filters__metadata {
    width: 100%;
    justify-content: flex-start;
    flex-wrap: wrap;
  }

  .queue-filters__metadata :deep(.app-button) {
    margin-inline-start: auto;
  }

  .queue-filters select {
    min-height: var(--pd-control-height-touch);
    font-size: var(--pd-font-size-16);
  }
}
</style>
