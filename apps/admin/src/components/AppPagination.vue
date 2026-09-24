<script setup lang="ts">
import { computed } from 'vue'
import { ChevronLeft, ChevronRight } from 'lucide-vue-next'

import AppButton from './AppButton.vue'

interface AppPaginationProps {
  page: number
  pageSize: number
  total: number | null
  hasNext: boolean
  hasPrev: boolean
  ariaLabel?: string
  prevLabel?: string
  nextLabel?: string
}

const props = withDefaults(defineProps<AppPaginationProps>(), {
  ariaLabel: '分页',
  prevLabel: '上一页',
  nextLabel: '下一页',
})

const emit = defineEmits<{
  (e: 'prev'): void
  (e: 'next'): void
}>()

const rangeLabel = computed<string>(() => {
  if (props.total === null) {
    return `第 ${props.page} 页 · 每页 ${props.pageSize}`
  }
  if (props.total === 0) {
    return '共 0 条'
  }
  const start = (props.page - 1) * props.pageSize + 1
  const end = Math.min(props.total, props.page * props.pageSize)
  return `第 ${start}–${end} 条 / 共 ${props.total} 条`
})
</script>

<template>
  <nav class="app-pagination" :aria-label="ariaLabel" data-testid="app-pagination">
    <AppButton size="sm" variant="ghost" :disabled="!hasPrev" @click="emit('prev')">
      <template #default>
        <ChevronLeft :size="14" aria-hidden="true" />
        <span>{{ prevLabel }}</span>
      </template>
    </AppButton>
    <span class="app-pagination__range tabular" aria-live="polite">{{ rangeLabel }}</span>
    <AppButton size="sm" variant="ghost" :disabled="!hasNext" @click="emit('next')">
      <template #default>
        <span>{{ nextLabel }}</span>
        <ChevronRight :size="14" aria-hidden="true" />
      </template>
    </AppButton>
  </nav>
</template>

<style scoped>
.app-pagination {
  display: flex;
  align-items: center;
  justify-content: space-between;
  min-height: var(--pd-control-height-md);
  gap: var(--pd-space-12);
  padding: var(--pd-space-12) 0;
}

.app-pagination__range {
  min-width: 0;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-metadata);
  text-align: center;
}

@media (max-width: 767px) {
  .app-pagination {
    align-items: stretch;
    flex-wrap: wrap;
  }

  .app-pagination__range {
    order: -1;
    flex-basis: 100%;
  }
}
</style>
