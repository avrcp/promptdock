<script setup lang="ts">
import { computed, onUnmounted, ref } from 'vue'
import { Check, Copy, AlertCircle } from 'lucide-vue-next'

import AppIconButton from './AppIconButton.vue'

interface CopyValueProps {
  value: string
  label?: string
  hideValue?: boolean
  ariaLabelCopy?: string
  monospace?: boolean
}

const props = withDefaults(defineProps<CopyValueProps>(), {
  label: '',
  hideValue: false,
  ariaLabelCopy: '复制',
  monospace: true,
})

const state = ref<'idle' | 'copied' | 'failed'>('idle')
let resetTimer: ReturnType<typeof setTimeout> | null = null

const displayValue = computed(() => {
  if (props.hideValue) return '••••••••'
  return props.value
})

function clearResetTimer(): void {
  if (resetTimer !== null) {
    clearTimeout(resetTimer)
    resetTimer = null
  }
}

onUnmounted(() => {
  clearResetTimer()
})

async function copy(): Promise<void> {
  clearResetTimer()
  if (typeof navigator === 'undefined' || !navigator.clipboard) {
    state.value = 'failed'
    resetTimer = setTimeout(() => {
      state.value = 'idle'
      resetTimer = null
    }, 2000)
    return
  }
  try {
    await navigator.clipboard.writeText(props.value)
    state.value = 'copied'
    resetTimer = setTimeout(() => {
      state.value = 'idle'
      resetTimer = null
    }, 2000)
  } catch {
    state.value = 'failed'
    resetTimer = setTimeout(() => {
      state.value = 'idle'
      resetTimer = null
    }, 2000)
  }
}
</script>

<template>
  <span class="copy-value" :class="{ 'copy-value--mono': monospace }">
    <span v-if="label" class="copy-value__label">{{ label }}</span>
    <span
      class="copy-value__text"
      data-testid="copy-value-text"
      :title="hideValue ? undefined : displayValue"
    >
      {{ displayValue }}
    </span>
    <AppIconButton
      :icon="state === 'copied' ? Check : state === 'failed' ? AlertCircle : Copy"
      size="sm"
      :ariaLabel="ariaLabelCopy"
      @click="copy"
    />
    <span v-if="state === 'failed'" class="copy-value__error">复制失败</span>
    <span class="sr-only" role="status" aria-live="polite">
      {{ state === 'copied' ? '已复制' : state === 'failed' ? '复制失败' : '' }}
    </span>
  </span>
</template>

<style scoped>
.copy-value {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-8);
  min-width: 0;
}

.copy-value__label {
  flex: 0 0 auto;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
}

.copy-value__text {
  flex: 1 1 auto;
  min-width: 0;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.copy-value--mono .copy-value__text {
  font-family: var(--pd-font-code);
}

.copy-value__error {
  flex: 0 0 auto;
  color: var(--pd-feedback-danger);
  font-size: var(--pd-font-size-11);
  white-space: nowrap;
}
</style>
