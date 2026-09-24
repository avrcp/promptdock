<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Ban, FileOutput, RefreshCw } from 'lucide-vue-next'

import AdminErrorAlert from '@/components/AdminErrorAlert.vue'
import AppButton from '@/components/AppButton.vue'
import AppDialog from '@/components/AppDialog.vue'
import AppTable from '@/components/AppTable.vue'
import EmptyState from '@/components/EmptyState.vue'
import SkeletonBlock from '@/components/SkeletonBlock.vue'
import StatusChip from '@/components/StatusChip.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import { useAdminCapabilities } from '@/composables/useAdminCapabilities'
import { useAdminRepository } from '@/composables/useAdminRepository'
import { useAsyncResource } from '@/composables/useAsyncResource'
import type { ResultListItem } from '@/contracts/result'

const repository = useAdminRepository()
const { can: canCapability, capabilityReason } = useAdminCapabilities()
const resource = useAsyncResource({
  immediate: false,
  fetcher: (signal) => repository.read.listResults({ limit: 50 }, { signal }),
})
const selected = ref<ResultListItem | null>(null)
const revokeRequestId = ref<string | null>(null)
const dialogOpen = computed({
  get: () => selected.value !== null,
  set: (value: boolean) => {
    if (!value) {
      selected.value = null
      revokeRequestId.value = null
    }
  },
})
const revoking = ref(false)
const mutationError = ref<Error | null>(null)
const canRevoke = computed(() => canCapability('admin_results_manage_v2'))
const revokeReason = computed(() => capabilityReason('admin_results_manage_v2'))
const tableRows = computed(() =>
  (resource.data.value?.items ?? []).map((item) => ({ ...item, id: item.resultRowId })),
)
onMounted(() => {
  void resource.refresh('initial')
})

const columns = [
  { key: 'safeTitle', label: '结果' },
  { key: 'pageState', label: '页面状态', width: '120px' },
  { key: 'notificationStatus', label: '通知状态', width: '130px' },
  { key: 'pageExpiresAt', label: '到期', width: '110px' },
  { key: 'acceptedAt', label: '接管', width: '110px' },
  { key: 'bodyBytes', label: '大小', align: 'right' as const, width: '78px' },
  { key: 'action', label: '操作', width: '84px' },
]

function pageVisual(state: ResultListItem['pageState']) {
  return {
    available: { label: '可访问', tone: 'success' as const },
    revoked: { label: '已撤销', tone: 'danger' as const },
    expired: { label: '已过期', tone: 'warning' as const },
    content_unavailable: { label: '内容不可用', tone: 'muted' as const },
  }[state]
}

function notificationLabel(value: ResultListItem['notificationStatus']) {
  return {
    pending_channel: '等待通道',
    sending_channel: '发送中',
    retry_wait: '等待重试',
    provider_accepted: '服务商已接收',
    blocked_activation: '等待激活',
    blocked_reconnect: '等待重连',
    dead_letter: '投递失败',
    expired: '通知已过期',
    cancelled: '已取消',
  }[value]
}

function bytes(value: number) {
  return value < 1024 ? `${value} B` : `${(value / 1024).toFixed(1)} KiB`
}

function requestRevoke(row: ResultListItem) {
  mutationError.value = null
  revokeRequestId.value = null
  selected.value = row
}

async function revoke() {
  if (!selected.value || !canRevoke.value) return
  revoking.value = true
  mutationError.value = null
  try {
    const requestId = revokeRequestId.value ?? crypto.randomUUID()
    revokeRequestId.value = requestId
    await repository.command.revokeResult({
      resultRowId: selected.value.resultRowId,
      requestId,
    })
    selected.value = null
    revokeRequestId.value = null
    await resource.refresh('refresh')
  } catch (error) {
    mutationError.value = error instanceof Error ? error : new Error('撤销失败，请稍后重试。')
  } finally {
    revoking.value = false
  }
}
</script>

<template>
  <div class="results-page">
    <div class="results-page__toolbar">
      <div>
        <h1>结果页</h1>
        <p>只显示安全投影；正文、分享令牌和链接不会进入 Admin。</p>
      </div>
      <AppButton
        variant="secondary"
        size="sm"
        :loading="resource.refreshing.value"
        @click="resource.refresh('refresh')"
      >
        <RefreshCw :size="14" aria-hidden="true" />
        刷新
      </AppButton>
    </div>
    <AdminErrorAlert
      v-if="resource.error.value"
      :error="resource.error.value"
      @retry="resource.refresh('refresh')"
    />
    <div v-if="resource.loading.value" class="results-page__skeleton" role="status">
      <SkeletonBlock v-for="index in 4" :key="index" variant="table-row" />
    </div>
    <EmptyState
      v-else-if="resource.data.value && resource.data.value.items.length === 0"
      :icon="FileOutput"
      title="还没有结果页"
      description="Relay 接管完整结果后，安全状态会显示在这里。"
    />
    <AppTable
      v-else
      :columns="columns"
      :rows="tableRows"
      :row-key="(row) => row.id"
      :loading="resource.refreshing.value"
      aria-label="结果页安全状态列表"
    >
      <template #cell-safeTitle="{ row }">
        <span class="results-page__title">{{ row.safeTitle }}</span>
      </template>
      <template #cell-pageState="{ row }">
        <StatusChip v-bind="pageVisual(row.pageState)" />
      </template>
      <template #cell-notificationStatus="{ row }">
        <span>{{ notificationLabel(row.notificationStatus) }}</span>
      </template>
      <template #cell-pageExpiresAt="{ row }"><TimeAgo :timestamp="row.pageExpiresAt" /></template>
      <template #cell-acceptedAt="{ row }"><TimeAgo :timestamp="row.acceptedAt" /></template>
      <template #cell-bodyBytes="{ row }">
        <span class="tabular">{{ bytes(row.bodyBytes) }}</span>
      </template>
      <template #cell-action="{ row }">
        <AppButton
          variant="danger"
          size="sm"
          :disabled="row.pageState !== 'available' || !canRevoke"
          :title="canRevoke ? undefined : revokeReason"
          @click="requestRevoke(row)"
        >
          <Ban :size="14" aria-hidden="true" />
          撤销
        </AppButton>
      </template>
    </AppTable>
    <p v-if="!canRevoke" class="results-page__capability">{{ revokeReason }}</p>
    <AppDialog
      v-model:open="dialogOpen"
      title="撤销结果页访问"
      description="撤销后分享页立即失效，尚未发送的链接通知会被取消。此操作不会删除原始结果记录。"
      primary-label="撤销访问"
      primary-variant="danger"
      :primary-loading="revoking"
      :pending="revoking"
      @primary="revoke"
    >
      <p v-if="mutationError" class="results-page__mutation-error">{{ mutationError.message }}</p>
    </AppDialog>
  </div>
</template>

<style scoped>
.results-page {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-16);
}
.results-page__toolbar {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--pd-space-16);
}
.results-page h1 {
  font-size: var(--pd-font-size-20);
  line-height: var(--pd-line-height-compact);
}
.results-page p {
  margin-top: var(--pd-space-4);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
}
.results-page__skeleton {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-8);
}
.results-page__title {
  display: block;
  max-width: 24ch;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.results-page__capability {
  color: var(--pd-text-muted);
}
.results-page__mutation-error {
  color: var(--pd-feedback-danger);
}
@media (max-width: 767px) {
  .results-page__toolbar {
    flex-direction: column;
    align-items: stretch;
  }
}
</style>
