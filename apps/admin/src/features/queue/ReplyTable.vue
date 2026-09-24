<script setup lang="ts">
import { MessageSquareReply } from 'lucide-vue-next'

import AppButton from '@/components/AppButton.vue'
import AppPagination from '@/components/AppPagination.vue'
import AppTable from '@/components/AppTable.vue'
import EmptyState from '@/components/EmptyState.vue'
import StatusChip from '@/components/StatusChip.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import type { InteractiveReplyListItem } from '@/contracts/queue'
import { replyStateVisual } from './queue-status'

defineProps<{
  rows: InteractiveReplyListItem[]
  loaded: boolean
  refreshing: boolean
  page: number
  total: number | null
  hasNext: boolean
  hasPrevious: boolean
  hasFilters: boolean
}>()

const emit = defineEmits<{
  open: [row: InteractiveReplyListItem]
  next: []
  previous: []
  clearFilters: []
}>()

const columns = [
  { key: 'commandRef', label: '关联命令' },
  { key: 'state', label: '状态', width: '110px' },
  { key: 'targetFingerprint', label: '目标账号' },
  { key: 'occurredAt', label: '时间', width: '110px' },
  { key: 'errorCode', label: '错误码', width: '160px' },
]
</script>

<template>
  <EmptyState
    v-if="loaded && rows.length === 0"
    :icon="MessageSquareReply"
    title="没有匹配的交互回复"
    :description="hasFilters ? '当前筛选条件下没有交互回复。' : '当前还没有任何交互回复。'"
  >
    <template v-if="hasFilters" #action>
      <AppButton variant="secondary" size="sm" @click="emit('clearFilters')">清空筛选</AppButton>
    </template>
  </EmptyState>
  <div v-else class="queue-table-shell">
    <AppTable
      :columns="columns"
      :rows="rows"
      :row-key="(row: InteractiveReplyListItem) => row.id"
      :loading="refreshing"
      :interactive-rows="false"
      :aria-label="`交互回复列表（${rows.length} 条）`"
      data-testid="reply-table"
    >
      <template #cell-commandRef="{ row }">
        <button
          type="button"
          class="queue__detail-link queue-table__detail-link"
          :aria-label="`查看交互回复 ${row.id} 详情`"
          @click="emit('open', row)"
        >
          <span class="queue-table__mono break-anywhere">{{ row.commandRef }}</span>
        </button>
      </template>
      <template #cell-targetFingerprint="{ row }">
        <span class="queue-table__mono break-anywhere">{{ row.targetFingerprint }}</span>
      </template>
      <template #cell-state="{ row }">
        <StatusChip
          :tone="replyStateVisual(row.state).tone"
          :label="replyStateVisual(row.state).label"
        />
      </template>
      <template #cell-occurredAt="{ row }"><TimeAgo :timestamp="row.occurredAt" /></template>
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

.queue-table__mono {
  color: var(--pd-text-muted);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-11);
}

.queue-table__error {
  color: var(--pd-feedback-danger);
  font-size: var(--pd-font-size-11);
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
