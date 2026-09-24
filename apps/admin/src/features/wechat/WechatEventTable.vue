<script setup lang="ts">
import EmptyState from '@/components/EmptyState.vue'
import SkeletonBlock from '@/components/SkeletonBlock.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import type { ChannelEventItem } from '@/contracts/wechat'

defineProps<{
  events: ChannelEventItem[]
  loading: boolean
}>()
</script>

<template>
  <section class="wechat-events" aria-labelledby="wechat-events-title">
    <header class="wechat-events__header">
      <h2 id="wechat-events-title">最近通道事件</h2>
    </header>
    <div v-if="loading" class="wechat-events__skeleton" role="status" aria-live="polite">
      <span class="sr-only">正在加载通道事件</span>
      <SkeletonBlock v-for="i in 3" :key="i" variant="table-row" />
    </div>
    <EmptyState
      v-else-if="events.length === 0"
      variant="bare"
      title="没有通道事件"
      description="尚未记录微信通道事件。"
    />
    <div v-else class="wechat-events__scroll">
      <table class="wechat-events__table">
        <thead>
          <tr>
            <th scope="col">时间</th>
            <th scope="col">事件</th>
            <th scope="col">安全详情</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="event in events" :key="event.id">
            <td><TimeAgo class="tabular" :timestamp="event.occurredAt" /></td>
            <td class="mono">{{ event.kind }}</td>
            <td class="wechat-events__message" :title="event.safeMessage">
              {{ event.safeMessage }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>

<style scoped>
.wechat-events {
  margin-top: var(--pd-space-16);
  overflow: hidden;
  background: var(--pd-container-panel-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.wechat-events__header {
  display: flex;
  align-items: center;
  min-height: var(--pd-panel-header-height);
  padding: 0 var(--pd-space-16);
  border-bottom: 1px solid var(--pd-border-separator);
  background: var(--pd-container-header-bg);
}

.wechat-events__header h2 {
  margin: 0;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-14);
  font-weight: var(--pd-font-weight-semibold);
}

.wechat-events__skeleton {
  display: grid;
  gap: var(--pd-space-12);
  padding: var(--pd-space-16);
}

.wechat-events__scroll {
  overflow-x: auto;
}

.wechat-events__table {
  width: 100%;
  min-width: 560px;
  border-collapse: collapse;
  table-layout: fixed;
}

.wechat-events__table th {
  height: var(--pd-table-header-height);
  padding: 0 var(--pd-space-12);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  font-weight: var(--pd-font-weight-medium);
  text-align: start;
}

.wechat-events__table td {
  height: var(--pd-table-row-height);
  padding: var(--pd-space-8) var(--pd-space-12);
  border-top: 1px solid var(--pd-border-separator);
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  vertical-align: middle;
}

.wechat-events__table th:first-child,
.wechat-events__table td:first-child {
  width: 124px;
}

.wechat-events__table th:nth-child(2),
.wechat-events__table td:nth-child(2) {
  width: 160px;
}

.wechat-events__message {
  display: -webkit-box;
  overflow: hidden;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 2;
  line-height: var(--pd-line-height-ui);
}
</style>
