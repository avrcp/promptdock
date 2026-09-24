<script setup lang="ts">
import { Inbox } from 'lucide-vue-next'

import AppButton from '@/components/AppButton.vue'
import AppPagination from '@/components/AppPagination.vue'
import AppTable from '@/components/AppTable.vue'
import EmptyState from '@/components/EmptyState.vue'
import StatusChip from '@/components/StatusChip.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import type { DeliveryListItem } from '@/contracts/queue'
import {
  deliveryKindVisual,
  deliveryOriginVisual,
  deliveryPriorityLabel,
  deliveryStateVisual,
} from './queue-status'

defineProps<{
  rows: DeliveryListItem[]
  loaded: boolean
  refreshing: boolean
  page: number
  total: number | null
  hasNext: boolean
  hasPrevious: boolean
  hasFilters: boolean
}>()

const emit = defineEmits<{
  open: [row: DeliveryListItem]
  next: []
  previous: []
  clearFilters: []
}>()

const columns = [
  { key: 'kind', label: '类型' },
  { key: 'state', label: '状态', width: '110px' },
  { key: 'origin', label: '来源', width: '148px' },
  { key: 'priorityLabel', label: '优先级', width: '76px' },
  { key: 'priorityValue', label: '值', align: 'right' as const, width: '52px' },
  { key: 'attemptCount', label: '尝试', align: 'right' as const, width: '70px' },
  { key: 'createdAt', label: '创建', width: '110px' },
  { key: 'updatedAt', label: '更新', width: '110px' },
  { key: 'errorCode', label: '错误码', width: '160px' },
]

function originLabel(row: DeliveryListItem): string {
  const type = deliveryOriginVisual(row.origin)
  return row.originLabel === type ? type : `${type} · ${row.originLabel}`
}
</script>

<template>
  <EmptyState
    v-if="loaded && rows.length === 0"
    :icon="Inbox"
    title="没有匹配的投递"
    :description="hasFilters ? '当前筛选条件下没有投递记录。' : '当前还没有任何通知投递。'"
  >
    <template v-if="hasFilters" #action>
      <AppButton variant="secondary" size="sm" @click="emit('clearFilters')">清空筛选</AppButton>
    </template>
  </EmptyState>
  <div v-else class="queue-table-shell">
    <AppTable
      :columns="columns"
      :rows="rows"
      :row-key="(row: DeliveryListItem) => row.id"
      :loading="refreshing"
      :interactive-rows="false"
      :aria-label="`通知投递列表（${rows.length} 条）`"
      data-testid="delivery-table"
    >
      <template #cell-kind="{ row }">
        <button
          type="button"
          class="queue__detail-link queue-table__detail-link"
          :aria-label="`查看投递 ${row.id} 详情`"
          @click="emit('open', row)"
        >
          {{ deliveryKindVisual(row.kind) }}
        </button>
      </template>
      <template #cell-origin="{ row }">
        <span class="queue-table__mono queue-table__origin" :title="originLabel(row)">
          {{ originLabel(row) }}
        </span>
      </template>
      <template #cell-state="{ row }">
        <StatusChip
          :tone="deliveryStateVisual(row.state).tone"
          :label="deliveryStateVisual(row.state).label"
        />
        <small v-if="row.segmentCount" class="tabular">
          已接收 {{ row.acceptedSegments ?? 0 }} / {{ row.segmentCount }} 段
        </small>
      </template>
      <template #cell-priorityLabel="{ row }">
        <span class="queue-table__priority-label">{{ deliveryPriorityLabel(row.priority) }}</span>
      </template>
      <template #cell-priorityValue="{ row }">
        <span class="tabular">{{ row.priority }}</span>
      </template>
      <template #cell-attemptCount="{ row }">
        <span class="tabular">{{ row.attemptCount }}</span>
      </template>
      <template #cell-createdAt="{ row }"><TimeAgo :timestamp="row.createdAt" /></template>
      <template #cell-updatedAt="{ row }"><TimeAgo :timestamp="row.updatedAt" /></template>
      <template #cell-errorCode="{ row }">
        <span class="queue-table__error mono break-anywhere">{{ row.errorCode ?? '—' }}</span>
      </template>
    </AppTable>
    <AppPagination
      :page="page"
      :page-size="50"
      :total="total"
      :has-next="hasNext"
      :has-prev="hasPrevious"
      @next="emit('next')"
      @prev="emit('previous')"
    />
  </div>
</template>

<style scoped>
.queue-table__detail-link {
  padding: 0;
  color: var(--pd-text-link);
  background: transparent;
  border: 0;
  font: inherit;
  text-align: start;
  cursor: pointer;
  text-decoration: underline;
  text-underline-offset: 2px;
}

.queue-table__detail-link:focus-visible {
  outline: 2px solid var(--pd-border-focus);
  outline-offset: 2px;
}

.queue-table__error {
  color: var(--pd-feedback-danger);
  font-size: var(--pd-font-size-11);
}

.queue-table__mono {
  color: var(--pd-text-muted);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-11);
}

.queue-table__origin {
  display: block;
  width: 148px;
  max-width: 148px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.queue-table__priority-label {
  white-space: nowrap;
}

.queue-table-shell :deep(.app-table) {
  border-end-start-radius: 0;
  border-end-end-radius: 0;
}

.queue-table-shell :deep(.app-pagination) {
  padding: var(--pd-space-8) var(--pd-space-12);
  background: var(--pd-container-header-bg);
  border: 1px solid var(--pd-border-separator);
  border-top: 0;
  border-end-start-radius: var(--pd-radius-md);
  border-end-end-radius: var(--pd-radius-md);
}
</style>
