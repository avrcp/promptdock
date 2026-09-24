<script setup lang="ts">
import { computed } from 'vue'

import { useNow } from '@/composables/useNow'

interface TimeAgoProps {
  timestamp: number | null
  fallback?: string
}

const props = withDefaults(defineProps<TimeAgoProps>(), {
  fallback: '从未',
})

// Share one 30s ticker across every TimeAgo instance in the app.
const { now } = useNow()

const label = computed<string>(() => {
  if (props.timestamp === null) return props.fallback
  const diff = now.value - props.timestamp
  if (diff < 0) return '即将'
  if (diff < 60_000) return '刚刚'
  if (diff < 60 * 60_000) return `${Math.floor(diff / 60_000)} 分钟前`
  if (diff < 24 * 60 * 60_000) return `${Math.floor(diff / (60 * 60_000))} 小时前`
  return `${Math.floor(diff / (24 * 60 * 60_000))} 天前`
})

const absoluteLabel = computed(() => {
  if (props.timestamp === null) return props.fallback
  try {
    return new Date(props.timestamp).toISOString()
  } catch {
    return String(props.timestamp)
  }
})
</script>

<template>
  <span class="time-ago tabular" :title="absoluteLabel" data-testid="time-ago">
    {{ label }}
  </span>
</template>

<style scoped>
.time-ago {
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  white-space: nowrap;
}
</style>
