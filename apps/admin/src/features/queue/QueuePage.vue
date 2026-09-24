<script setup lang="ts">
import { computed } from 'vue'
import { RefreshCw } from 'lucide-vue-next'

import AdminErrorAlert from '@/components/AdminErrorAlert.vue'
import AppButton from '@/components/AppButton.vue'
import AppTabs from '@/components/AppTabs.vue'
import SkeletonBlock from '@/components/SkeletonBlock.vue'
import DeliveryTable from './DeliveryTable.vue'
import InboundTable from './InboundTable.vue'
import QueueDetailDrawer from './QueueDetailDrawer.vue'
import QueueFilters from './QueueFilters.vue'
import ReplyTable from './ReplyTable.vue'
import { QUEUE_TABS, useQueueController } from './useQueueController'

const controller = useQueueController()
const {
  activeTab,
  deliveryState,
  deliveryOrigin,
  replyState,
  inboundState,
  inboundCommand,
  deliveriesResource,
  repliesResource,
  inboundResource,
  deliveryRows,
  replyRows,
  inboundRows,
  activeError,
  activeLoading,
  activeRefreshing,
  activeHasUsableData,
  refreshActive,
  nextDelivery,
  previousDelivery,
  nextReply,
  previousReply,
  nextInbound,
  previousInbound,
  deliveryPager,
  replyPager,
  inboundPager,
  deliveryFreshness,
  replyFreshness,
  inboundFreshness,
  selectedRow,
  selectedItemMissing,
  drawerOpen,
  openDelivery,
  openReply,
  openInbound,
  clearActiveFilters,
} = controller

const activeFreshness = computed(() => {
  if (activeTab.value === 'deliveries') return deliveryFreshness.value
  if (activeTab.value === 'replies') return replyFreshness.value
  return inboundFreshness.value
})
const freshnessTestid = computed(() => {
  if (activeTab.value === 'deliveries') return 'delivery-freshness'
  if (activeTab.value === 'replies') return 'reply-freshness'
  return 'inbound-freshness'
})
const activeCount = computed(() => {
  if (activeTab.value === 'deliveries') return deliveryRows.value.length
  if (activeTab.value === 'replies') return replyRows.value.length
  return inboundRows.value.length
})
const activeTotal = computed(() => {
  if (activeTab.value === 'deliveries') return deliveriesResource.data.value?.total ?? null
  if (activeTab.value === 'replies') return repliesResource.data.value?.total ?? null
  return inboundResource.data.value?.total ?? null
})
const activeHasFilters = computed(() => {
  if (activeTab.value === 'deliveries') {
    return deliveryState.value !== null || deliveryOrigin.value !== null
  }
  if (activeTab.value === 'replies') return replyState.value !== null
  return inboundState.value !== null || inboundCommand.value !== null
})
const activeFreshnessIsStale = computed(
  () => activeFreshness.value.includes('失败') || activeFreshness.value.includes('过期'),
)
</script>

<template>
  <div class="queue-page">
    <div class="queue-page__toolbar" role="toolbar" aria-label="队列工具">
      <AppTabs
        v-model="activeTab"
        :tabs="QUEUE_TABS"
        aria-label="队列分区"
        base-id="queue"
        data-testid="queue-tabs"
      />
      <AppButton
        variant="secondary"
        size="sm"
        :loading="activeRefreshing"
        data-testid="queue-refresh"
        @click="refreshActive"
      >
        <template #default>
          <RefreshCw :size="14" aria-hidden="true" />
          <span>刷新</span>
        </template>
      </AppButton>
    </div>

    <AdminErrorAlert
      v-if="activeError"
      :error="activeError"
      testid="queue-error"
      @retry="refreshActive"
    />

    <div
      v-if="activeLoading"
      class="queue-page__skeleton"
      data-testid="queue-skeleton"
      role="status"
      aria-live="polite"
    >
      <span class="sr-only">正在加载队列数据</span>
      <SkeletonBlock v-for="index in 4" :key="index" variant="table-row" />
    </div>
    <div
      v-else-if="activeError && !activeHasUsableData"
      class="queue-page__initial-error"
      data-testid="queue-initial-error"
    >
      <p>当前无法确认此分区是否为空，请使用上方恢复操作重试。</p>
    </div>
    <div
      v-else
      :id="`queue-panel-${activeTab}`"
      class="queue-page__panel"
      role="tabpanel"
      :aria-labelledby="`queue-tab-${activeTab}`"
    >
      <QueueFilters
        v-model:delivery-state="deliveryState"
        v-model:delivery-origin="deliveryOrigin"
        v-model:reply-state="replyState"
        v-model:inbound-state="inboundState"
        v-model:inbound-command="inboundCommand"
        :tab="activeTab"
        :freshness="activeFreshness"
        :freshness-testid="freshnessTestid"
        :freshness-stale="activeFreshnessIsStale"
        :result-count="activeCount"
        :total="activeTotal"
        :has-filters="activeHasFilters"
        @clear-filters="clearActiveFilters"
      />

      <DeliveryTable
        v-if="activeTab === 'deliveries'"
        :rows="deliveryRows"
        :loaded="deliveriesResource.data.value !== null"
        :refreshing="deliveriesResource.refreshing.value"
        :page="deliveryPager.page.value"
        :total="deliveriesResource.data.value?.total ?? null"
        :has-next="(deliveriesResource.data.value?.nextCursor ?? null) !== null"
        :has-previous="deliveryPager.hasPrevious.value"
        :has-filters="deliveryState !== null || deliveryOrigin !== null"
        @open="openDelivery"
        @next="nextDelivery"
        @previous="previousDelivery"
        @clear-filters="clearActiveFilters"
      />
      <ReplyTable
        v-else-if="activeTab === 'replies'"
        :rows="replyRows"
        :loaded="repliesResource.data.value !== null"
        :refreshing="repliesResource.refreshing.value"
        :page="replyPager.page.value"
        :total="repliesResource.data.value?.total ?? null"
        :has-next="(repliesResource.data.value?.nextCursor ?? null) !== null"
        :has-previous="replyPager.hasPrevious.value"
        :has-filters="replyState !== null"
        @open="openReply"
        @next="nextReply"
        @previous="previousReply"
        @clear-filters="clearActiveFilters"
      />
      <InboundTable
        v-else
        :rows="inboundRows"
        :loaded="inboundResource.data.value !== null"
        :refreshing="inboundResource.refreshing.value"
        :page="inboundPager.page.value"
        :total="inboundResource.data.value?.total ?? null"
        :has-next="(inboundResource.data.value?.nextCursor ?? null) !== null"
        :has-previous="inboundPager.hasPrevious.value"
        :has-filters="inboundState !== null || inboundCommand !== null"
        @open="openInbound"
        @next="nextInbound"
        @previous="previousInbound"
        @clear-filters="clearActiveFilters"
      />
    </div>

    <QueueDetailDrawer
      v-model:open="drawerOpen"
      :selected="selectedRow"
      :missing="selectedItemMissing"
    />
  </div>
</template>

<style scoped>
.queue-page__toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-12);
}

.queue-page__panel,
.queue-page__skeleton {
  margin-top: var(--pd-space-16);
}

.queue-page__skeleton {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-8);
}

.queue-page__initial-error {
  margin-top: var(--pd-space-16);
  padding: var(--pd-space-16);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-reading);
  background: var(--pd-container-panel-bg);
  border-inline-start: 2px solid var(--pd-feedback-danger);
}

@media (max-width: 767px) {
  .queue-page__toolbar {
    align-items: stretch;
    flex-direction: column;
    gap: var(--pd-space-8);
  }
}
</style>
