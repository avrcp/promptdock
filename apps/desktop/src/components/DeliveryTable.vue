<script setup lang="ts">
import { computed } from 'vue'
import StatusChip from './StatusChip.vue'
import type { StatusTone } from './StatusChip.vue'
import {
  deliveryIso,
  deliveryLabel,
  deliveryTone,
  formatDeliveryTime,
  resultPageLabel,
  type Delivery,
} from '../desktop/dictionaries'

interface DeliveryTableProps {
  items: readonly Delivery[]
  caption: string
  /** 只渲染最近 N 条；不传则全部渲染。 */
  limit?: number
  /** 是否显示投递 ID（总览省略，详情页展示）。 */
  showId?: boolean
  /** 当前在结果面板中选中的稳定投递 ID。 */
  selectedId?: string | null
}

const props = withDefaults(defineProps<Readonly<DeliveryTableProps>>(), {
  limit: undefined,
  showId: false,
  selectedId: null,
})
defineEmits<{
  openResult: [item: Delivery]
  localBody: [item: Delivery]
}>()

const rows = computed(() => (props.limit ? props.items.slice(0, props.limit) : props.items))

function canOpenResult(item: Delivery) {
  return (
    item.result?.pageState === 'available' &&
    (item.result.pageExpiresAt ?? 0) > Date.now()
  )
}

function statusLabel(item: Delivery): string {
  const raw = item.result?.notificationStatus ?? item.remoteStatus ?? item.status
  if (
    item.status === 'blocked_reconnect' &&
    item.lastErrorCode === 'RELAY_RESULT_PAGES_UNAVAILABLE'
  ) {
    return '等待服务器结果页能力'
  }
  return deliveryLabel(raw)
}

function statusTone(item: Delivery): StatusTone {
  return deliveryTone(item.result?.notificationStatus ?? item.remoteStatus ?? item.status)
}
</script>

<template>
  <div class="table-scroll">
    <table class="data-table">
      <caption class="sr-only">
        {{
          caption
        }}
      </caption>
      <thead>
        <tr>
          <th scope="col">通知</th>
          <th scope="col">当前状态</th>
          <th scope="col">时间</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="item in rows"
          :key="item.id"
          :class="{ 'data-table__row--selected': showId && item.id === selectedId }"
        >
          <td>
            <span class="data-table__primary">
              <span v-if="showId && item.id === selectedId" class="data-table__selection">
                当前详情
              </span>
              <!-- 截断内容必须保留取回完整值的途径 -->
              <span class="data-table__title" :title="item.payload.title">
                {{ item.payload.title }}
              </span>
              <small v-if="showId" class="data-table__id mono" :title="item.id">{{
                item.id
              }}</small>
              <span v-if="showId" class="data-table__actions">
                <button
                  data-action="open-result"
                  class="button button--primary"
                  type="button"
                  :disabled="!canOpenResult(item)"
                  @click="$emit('openResult', item)"
                >
                  查看结果
                </button>
                <button
                  data-action="local-body"
                  class="button button--secondary"
                  type="button"
                  @click="$emit('localBody', item)"
                >
                  查看本地原文
                </button>
              </span>
            </span>
          </td>
          <td>
            <p v-if="item.result" class="field__hint">结果页：{{ resultPageLabel(item.result) }}</p>
            <StatusChip
              :tone="statusTone(item)"
              :label="
                (item.result ? '微信通知：' : '') +
                statusLabel(item)
              "
            />
            <p v-if="item.lastErrorCode === 'RELAY_RESULT_PAGES_UNAVAILABLE'" class="field__hint">
              当前连接未确认 result_pages_v1，原文保留在本机等待发布。
            </p>
          </td>
          <td>
            <time class="data-table__time numeric" :datetime="deliveryIso(item.createdAt)">
              {{ formatDeliveryTime(item.createdAt) }}
            </time>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.data-table__actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  justify-self: start;
}

.data-table__row--selected {
  background: var(--brand-soft);
}

.data-table__selection {
  color: var(--brand-primary);
  font-size: var(--font-size-meta);
  font-weight: var(--weight-semibold);
}
</style>
